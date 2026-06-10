//! Opt-in wall-clock timing per [`SimulationPhase`] (#381).
//!
//! [`install_phase_timing`] adds 11 boundary marker systems between the
//! chained phase sets. Each marker only stamps `Instant::now()` into the
//! [`PhaseTimings`] resource — no `Commands`, no deferred params, so the
//! schedule gains no new sync points and simulation behavior stays
//! unchanged (the timestamps are never read by any simulation system).
//!
//! The daemon reads the resource after `schedule.run()` and records the
//! durations into `sentinel-telemetry` histograms; this crate stays free of
//! a telemetry dependency.

use std::time::Instant;

use bevy_ecs::prelude::*;
use bevy_ecs::schedule::Schedule;

use crate::systems::SimulationPhase;

/// Number of simulation phases (must match [`SimulationPhase`]).
pub const PHASE_COUNT: usize = 10;

/// Phase label names, in the exact `.chain()` order of `create_simulation_world`.
pub const PHASE_NAMES: [&str; PHASE_COUNT] = [
    "input",
    "biology",
    "physics",
    "transit",
    "chaos",
    "mood",
    "perception",
    "decision",
    "output",
    "persist",
];

/// Wall-clock marks around each phase of the current tick.
///
/// `marks[i]` is stamped before phase `i` starts, `marks[PHASE_COUNT]` after
/// the last phase. Telemetry-only: no simulation system reads this resource.
#[derive(Resource, Default)]
pub struct PhaseTimings {
    marks: [Option<Instant>; PHASE_COUNT + 1],
}

impl PhaseTimings {
    /// Duration of phase `i` in milliseconds for the last completed tick.
    /// `None` until both surrounding marks have been stamped.
    pub fn duration_ms(&self, i: usize) -> Option<f64> {
        let start = self.marks.get(i).copied().flatten()?;
        let end = self.marks.get(i + 1).copied().flatten()?;
        Some(end.duration_since(start).as_secs_f64() * 1000.0)
    }
}

fn mark<const I: usize>(mut timings: ResMut<PhaseTimings>) {
    timings.marks[I] = Some(Instant::now());
}

/// Installs the [`PhaseTimings`] resource and the 11 boundary markers.
///
/// Opt-in on purpose: `create_simulation_world()` stays unchanged so tests
/// and benches without timing keep their exact schedule.
pub fn install_phase_timing(world: &mut World, schedule: &mut Schedule) {
    use SimulationPhase as P;
    world.insert_resource(PhaseTimings::default());
    schedule.add_systems((
        mark::<0>.before(P::Input),
        mark::<1>.after(P::Input).before(P::Biology),
        mark::<2>.after(P::Biology).before(P::Physics),
        mark::<3>.after(P::Physics).before(P::Transit),
        mark::<4>.after(P::Transit).before(P::Chaos),
        mark::<5>.after(P::Chaos).before(P::Mood),
        mark::<6>.after(P::Mood).before(P::Perception),
        mark::<7>.after(P::Perception).before(P::Decision),
        mark::<8>.after(P::Decision).before(P::Output),
        mark::<9>.after(P::Output).before(P::Persist),
        mark::<10>.after(P::Persist),
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::create_simulation_world;

    #[test]
    fn install_populates_all_phase_durations_after_one_tick() {
        let (mut world, mut schedule) = create_simulation_world();
        install_phase_timing(&mut world, &mut schedule);

        schedule.run(&mut world);

        let timings = world
            .get_resource::<PhaseTimings>()
            .expect("PhaseTimings resource installed");
        for (i, name) in PHASE_NAMES.iter().enumerate() {
            let ms = timings.duration_ms(i);
            assert!(
                ms.is_some(),
                "phase {name} (index {i}) has no duration after a tick"
            );
            assert!(ms.unwrap() >= 0.0, "phase {name} duration must be >= 0");
        }
        // Marks are monotonic, so the sum of phases never exceeds outer..inner span.
        assert!(timings.duration_ms(PHASE_COUNT).is_none());
    }

    #[test]
    fn without_install_no_resource_exists() {
        let (mut world, mut schedule) = create_simulation_world();
        schedule.run(&mut world);
        assert!(world.get_resource::<PhaseTimings>().is_none());
    }

    #[test]
    fn phase_names_match_phase_count() {
        assert_eq!(PHASE_NAMES.len(), PHASE_COUNT);
    }

    /// Determinism guard: the boundary markers must not change simulation
    /// results. Two identical worlds (one with timing installed) produce the
    /// same bio state, position and event count after two real ticks.
    #[test]
    fn install_does_not_change_simulation_results() {
        fn run_two_ticks(install: bool) -> (Vec<(f32, f32, f32, String)>, usize) {
            let (mut world, mut schedule) = create_simulation_world();
            if install {
                install_phase_timing(&mut world, &mut schedule);
            }
            crate::world::spawn_agent(
                &mut world,
                sentinel_common::AgentId(1),
                "Det-Guard",
                "dev",
                1,
                "empfang",
            );
            for tick in 1..=2u64 {
                if let Some(mut time) = world.get_resource_mut::<crate::SimulationTime>() {
                    time.tick = sentinel_common::Tick(tick);
                    time.tick_count = tick;
                    time.delta_seconds = 1.0;
                }
                schedule.run(&mut world);
            }
            let mut bio_state = Vec::new();
            let mut query = world.query::<(&crate::BioState, &crate::Position)>();
            for (bio, pos) in query.iter(&world) {
                bio_state.push((bio.hunger, bio.energy, bio.stress, pos.room_id.clone()));
            }
            let events = world
                .get_resource::<crate::EventBuffer>()
                .map(|b| b.events.len())
                .unwrap_or(0);
            (bio_state, events)
        }

        let baseline = run_two_ticks(false);
        let with_timing = run_two_ticks(true);
        assert_eq!(baseline, with_timing, "markers must not alter simulation");
    }
}
