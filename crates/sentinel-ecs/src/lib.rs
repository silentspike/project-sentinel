//! ECS world simulation core using bevy_ecs.
//!
//! Definiert 10 Components, 9 Systems (in strikter Reihenfolge via SimulationPhase),
//! und die World-Setup-Logik fuer die Agent-Simulation.

pub mod components;
pub mod systems;
pub mod world;

pub use components::*;
pub use systems::SimulationPhase;
pub use world::{create_simulation_world, spawn_agent};

#[cfg(test)]
mod tests {
    use super::*;
    use sentinel_common::AgentId;

    #[test]
    fn test_single_agent_spawn() {
        let (mut world, _schedule) = create_simulation_world();
        let entity = spawn_agent(&mut world, AgentId(1), "Thomas Mueller", "CEO", 1);

        // Alle 10 Components muessen vorhanden sein
        assert!(world.get::<AgentIdentity>(entity).is_some());
        assert!(world.get::<Position>(entity).is_some());
        assert!(world.get::<BioState>(entity).is_some());
        assert!(world.get::<Personality>(entity).is_some());
        assert!(world.get::<Mood>(entity).is_some());
        assert!(world.get::<Perception>(entity).is_some());
        assert!(world.get::<WorkContext>(entity).is_some());
        assert!(world.get::<Relationships>(entity).is_some());
        assert!(world.get::<LlmConfig>(entity).is_some());
        assert!(world.get::<ShiftInfo>(entity).is_some());

        // Shift-Werte pruefen (Set 1 = Frueh 06-14)
        let shift = world.get::<ShiftInfo>(entity).unwrap();
        assert_eq!(shift.shift_set, 1);
        assert_eq!(shift.shift_start_hour, 6);
        assert_eq!(shift.shift_end_hour, 14);

        // Identity pruefen
        let identity = world.get::<AgentIdentity>(entity).unwrap();
        assert_eq!(identity.agent_id, AgentId(1));
        assert_eq!(identity.name, "Thomas Mueller");
    }

    #[test]
    fn test_full_shift_15_agents() {
        let (mut world, _schedule) = create_simulation_world();
        let mut entities = Vec::new();

        for i in 1..=15 {
            let entity = spawn_agent(
                &mut world,
                AgentId(i),
                &format!("Agent-{:02}", i),
                "Mitarbeiter",
                1,
            );
            entities.push(entity);
        }

        // 15 einzigartige Entities
        assert_eq!(entities.len(), 15);
        let unique: std::collections::HashSet<_> = entities.iter().collect();
        assert_eq!(unique.len(), 15);

        // Alle haben AgentIdentity
        for entity in &entities {
            assert!(world.get::<AgentIdentity>(*entity).is_some());
        }
    }

    #[test]
    fn test_100_ticks_no_panic() {
        let (mut world, mut schedule) = create_simulation_world();

        // 15 Agenten spawnen (volle Schicht)
        for i in 1..=15 {
            spawn_agent(
                &mut world,
                AgentId(i),
                &format!("Agent-{:02}", i),
                "Mitarbeiter",
                1,
            );
        }

        // 100 Ticks ohne Panic
        for _ in 0..100 {
            schedule.run(&mut world);
        }
    }

    #[test]
    fn test_tick_rate_performance() {
        let (mut world, mut schedule) = create_simulation_world();

        for i in 1..=15 {
            spawn_agent(
                &mut world,
                AgentId(i),
                &format!("Agent-{:02}", i),
                "Mitarbeiter",
                1,
            );
        }

        let start = std::time::Instant::now();
        for _ in 0..100 {
            schedule.run(&mut world);
        }
        let elapsed = start.elapsed();

        // >100 ticks/s = 100 ticks in unter 1 Sekunde
        assert!(
            elapsed.as_secs_f64() < 1.0,
            "100 ticks took {:.3}s (must be < 1.0s for >100 ticks/s)",
            elapsed.as_secs_f64()
        );
    }
}
