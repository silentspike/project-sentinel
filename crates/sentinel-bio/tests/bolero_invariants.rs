//! Property-Based Tests fuer Bio-Engine Invarianten (bolero).
//!
//! Verifiziert dass alle biologischen Zustaende nach `update_bio_state()`
//! innerhalb ihrer definierten Grenzen bleiben, unabhaengig von den Eingaben.

use bolero::check;
use sentinel_bio::update_bio_state;
use sentinel_common::components::{BioState, Personality, WorkContext};

/// Sanitize f32: NaN/Inf → default_val, dann clamp.
fn san(v: f32, min: f32, max: f32, default: f32) -> f32 {
    if v.is_finite() {
        v.clamp(min, max)
    } else {
        default
    }
}

/// Erzeugt BioState aus beliebigen f32-Werten (NaN-safe, geclampt).
fn make_bio(vals: &(f32, f32, f32, f32, f32, f32, f32)) -> BioState {
    BioState {
        hunger: san(vals.0, 0.0, 100.0, 50.0),
        energy: san(vals.1, 0.0, 100.0, 50.0),
        caffeine_mg: if vals.2.is_finite() {
            vals.2.abs().min(500.0)
        } else {
            0.0
        },
        bladder: san(vals.3, 0.0, 100.0, 50.0),
        stress: san(vals.4, 0.0, 100.0, 50.0),
        social_need: san(vals.5, 0.0, 100.0, 50.0),
        comfort: san(vals.6, 0.0, 100.0, 50.0),
    }
}

/// Erzeugt Personality + WorkContext + Sim-Parameter aus beliebigen Werten.
fn make_context(
    vals: &(f32, f32, f32, f32, f32, bool, bool, bool, bool, f32, f32),
) -> (Personality, WorkContext, f32, f32) {
    let personality = Personality {
        openness: 0.5,
        conscientiousness: san(vals.0, 0.0, 1.0, 0.5),
        extraversion: san(vals.1, 0.0, 1.0, 0.5),
        agreeableness: 0.5,
        neuroticism: san(vals.2, 0.0, 1.0, 0.5),
        caffeine_tolerance: san(vals.3, 0.0, 1.0, 0.5),
        is_morning_person: vals.5,
    };
    let work = WorkContext {
        current_task: None,
        in_meeting: vals.6,
        has_deadline: vals.7,
        has_conflict: vals.8,
        conflict_cooldown: 0,
    };
    let dt_sec = if vals.9.is_finite() {
        vals.9.abs().clamp(0.001, 3600.0)
    } else {
        1.0
    };
    let sim_hour = if vals.10.is_finite() {
        vals.10.abs() % 24.0
    } else {
        12.0
    };
    (personality, work, dt_sec, sim_hour)
}

#[test]
fn hunger_always_in_bounds() {
    check!()
        .with_type::<(
            (f32, f32, f32, f32, f32, f32, f32),
            (f32, f32, f32, f32, f32, bool, bool, bool, bool, f32, f32),
        )>()
        .for_each(|(bio_vals, ctx_vals)| {
            let mut bio = make_bio(bio_vals);
            let (personality, work, dt_sec, sim_hour) = make_context(ctx_vals);

            update_bio_state(&mut bio, &personality, &work, dt_sec, sim_hour);

            assert!(
                bio.hunger >= 0.0 && bio.hunger <= 100.0,
                "hunger out of bounds: {} (dt={}, sim_hour={})",
                bio.hunger,
                dt_sec,
                sim_hour
            );
        });
}

#[test]
fn energy_always_in_bounds() {
    check!()
        .with_type::<(
            (f32, f32, f32, f32, f32, f32, f32),
            (f32, f32, f32, f32, f32, bool, bool, bool, bool, f32, f32),
        )>()
        .for_each(|(bio_vals, ctx_vals)| {
            let mut bio = make_bio(bio_vals);
            let (personality, work, dt_sec, sim_hour) = make_context(ctx_vals);

            update_bio_state(&mut bio, &personality, &work, dt_sec, sim_hour);

            assert!(
                bio.energy >= 0.0 && bio.energy <= 100.0,
                "energy out of bounds: {} (dt={}, sim_hour={})",
                bio.energy,
                dt_sec,
                sim_hour
            );
        });
}

#[test]
fn bladder_always_in_bounds() {
    check!()
        .with_type::<(
            (f32, f32, f32, f32, f32, f32, f32),
            (f32, f32, f32, f32, f32, bool, bool, bool, bool, f32, f32),
        )>()
        .for_each(|(bio_vals, ctx_vals)| {
            let mut bio = make_bio(bio_vals);
            let (personality, work, dt_sec, sim_hour) = make_context(ctx_vals);

            update_bio_state(&mut bio, &personality, &work, dt_sec, sim_hour);

            assert!(
                bio.bladder >= 0.0 && bio.bladder <= 100.0,
                "bladder out of bounds: {} (dt={}, sim_hour={})",
                bio.bladder,
                dt_sec,
                sim_hour
            );
        });
}

#[test]
fn stress_always_in_bounds() {
    check!()
        .with_type::<(
            (f32, f32, f32, f32, f32, f32, f32),
            (f32, f32, f32, f32, f32, bool, bool, bool, bool, f32, f32),
        )>()
        .for_each(|(bio_vals, ctx_vals)| {
            let mut bio = make_bio(bio_vals);
            let (personality, work, dt_sec, sim_hour) = make_context(ctx_vals);

            update_bio_state(&mut bio, &personality, &work, dt_sec, sim_hour);

            assert!(
                bio.stress >= 0.0 && bio.stress <= 100.0,
                "stress out of bounds: {} (dt={}, sim_hour={})",
                bio.stress,
                dt_sec,
                sim_hour
            );
        });
}

#[test]
fn social_need_always_in_bounds() {
    check!()
        .with_type::<(
            (f32, f32, f32, f32, f32, f32, f32),
            (f32, f32, f32, f32, f32, bool, bool, bool, bool, f32, f32),
        )>()
        .for_each(|(bio_vals, ctx_vals)| {
            let mut bio = make_bio(bio_vals);
            let (personality, work, dt_sec, sim_hour) = make_context(ctx_vals);

            update_bio_state(&mut bio, &personality, &work, dt_sec, sim_hour);

            assert!(
                bio.social_need >= 0.0 && bio.social_need <= 100.0,
                "social_need out of bounds: {} (dt={}, sim_hour={})",
                bio.social_need,
                dt_sec,
                sim_hour
            );
        });
}

#[test]
fn caffeine_always_in_bounds() {
    check!()
        .with_type::<(
            (f32, f32, f32, f32, f32, f32, f32),
            (f32, f32, f32, f32, f32, bool, bool, bool, bool, f32, f32),
        )>()
        .for_each(|(bio_vals, ctx_vals)| {
            let mut bio = make_bio(bio_vals);
            let (personality, work, dt_sec, sim_hour) = make_context(ctx_vals);

            update_bio_state(&mut bio, &personality, &work, dt_sec, sim_hour);

            assert!(
                bio.caffeine_mg >= 0.0 && bio.caffeine_mg <= 500.0,
                "caffeine_mg out of bounds: {} (dt={}, sim_hour={})",
                bio.caffeine_mg,
                dt_sec,
                sim_hour
            );
        });
}
