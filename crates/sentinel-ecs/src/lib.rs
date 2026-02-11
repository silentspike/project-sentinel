//! ECS world simulation core using bevy_ecs.
//!
//! Definiert 10 Components, 9 Systems (in strikter Reihenfolge via SimulationPhase),
//! und die World-Setup-Logik fuer die Agent-Simulation.
//!
//! Components liegen in sentinel-common::components (Re-Export hier).
//! Systems rufen sentinel-bio und sentinel-physics fuer echte Berechnungen auf.

pub mod components;
pub mod perception;
pub mod systems;
pub mod world;

pub use components::*;
pub use perception::{format_injection, generate_perception, PerceptionTexts, SmellEvent};
pub use systems::SimulationPhase;
pub use world::{create_simulation_world, spawn_agent, SimulationTime};

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
        assert!(world.get::<PerceptionState>(entity).is_some());
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

        // 100 Ticks ohne Panic - SimulationTime wird pro Tick aktualisiert
        for tick in 0..100u64 {
            let mut time = world.resource_mut::<SimulationTime>();
            time.tick = sentinel_common::Tick(tick);
            time.tick_count = tick;
            time.delta_seconds = 1.0;
            time.sim_hour = 8.0 + (tick as f32 / 3600.0);
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
        for tick in 0..100u64 {
            let mut time = world.resource_mut::<SimulationTime>();
            time.tick = sentinel_common::Tick(tick);
            time.tick_count = tick;
            time.delta_seconds = 1.0;
            time.sim_hour = 8.0 + (tick as f32 / 3600.0);
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

    #[test]
    fn test_bio_system_updates_hunger() {
        let (mut world, mut schedule) = create_simulation_world();
        let entity = spawn_agent(&mut world, AgentId(1), "Test Agent", "Tester", 1);

        // Initialer Hunger
        let initial_hunger = world.get::<BioState>(entity).unwrap().hunger;

        // 60 Ticks a 60 Sekunden = 1 Stunde
        for tick in 0..60u64 {
            let mut time = world.resource_mut::<SimulationTime>();
            time.tick = sentinel_common::Tick(tick);
            time.delta_seconds = 60.0; // 1 Minute pro Tick
            time.sim_hour = 10.0;
            schedule.run(&mut world);
        }

        let final_hunger = world.get::<BioState>(entity).unwrap().hunger;
        // 12.5/h → nach 1h sollte Hunger um ~12.5 gestiegen sein
        assert!(
            final_hunger > initial_hunger + 10.0,
            "Hunger should increase: initial={}, final={}",
            initial_hunger,
            final_hunger
        );
    }

    #[test]
    fn test_mood_system_calculates_valence() {
        let (mut world, mut schedule) = create_simulation_world();
        let entity = spawn_agent(&mut world, AgentId(1), "Test Agent", "Tester", 1);

        // Einen Tick ausfuehren
        {
            let mut time = world.resource_mut::<SimulationTime>();
            time.delta_seconds = 1.0;
            time.sim_hour = 10.0;
        }
        schedule.run(&mut world);

        // Mood sollte berechnet worden sein (nicht mehr Default 0.2)
        let mood = world.get::<Mood>(entity).unwrap();
        // Bei Energy=80, Stress=15 → positiver Valenz
        assert!(
            mood.valence > 0.0,
            "Valence should be positive for healthy agent, got {}",
            mood.valence
        );
    }

    #[test]
    fn test_perception_generates_text() {
        let (mut world, mut schedule) = create_simulation_world();
        let entity = spawn_agent(&mut world, AgentId(1), "Test Agent", "Tester", 1);

        // Einen Tick ausfuehren
        {
            let mut time = world.resource_mut::<SimulationTime>();
            time.tick = sentinel_common::Tick(1);
            time.delta_seconds = 1.0;
            time.sim_hour = 10.0;
        }
        schedule.run(&mut world);

        let perception = world.get::<PerceptionState>(entity).unwrap();
        // Environment-Text sollte gesetzt sein
        assert!(
            !perception.environment_text.is_empty(),
            "Environment text should not be empty"
        );
        assert!(
            perception.environment_text.contains("Empfang"),
            "Should mention Empfangsbereich, got: {}",
            perception.environment_text
        );
        assert_eq!(perception.last_updated, sentinel_common::Tick(1));
    }

    #[test]
    fn test_transit_system_completes_movement() {
        let (mut world, mut schedule) = create_simulation_world();
        let entity = spawn_agent(&mut world, AgentId(1), "Test Agent", "Tester", 1);

        // Agent in Transit setzen
        {
            let mut pos = world.get_mut::<Position>(entity).unwrap();
            pos.in_transit = true;
            pos.transit_target = Some("kueche".to_string());
            pos.transit_remaining_ms = 2000; // 2 Sekunden
        }

        // 3 Ticks a 1 Sekunde (= 3000ms > 2000ms Transit)
        for tick in 0..3u64 {
            let mut time = world.resource_mut::<SimulationTime>();
            time.tick = sentinel_common::Tick(tick);
            time.delta_seconds = 1.0;
            time.sim_hour = 10.0;
            schedule.run(&mut world);
        }

        let pos = world.get::<Position>(entity).unwrap();
        assert!(!pos.in_transit, "Transit should be complete");
        assert_eq!(pos.room_id, "kueche", "Should be in target room");
    }
}
