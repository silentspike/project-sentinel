//! Property-Based Tests fuer Physics-Engine Invarianten (bolero).
//!
//! Verifiziert dass Laermpegel und CO2-Konzentration nie unter ihre
//! physikalischen Minima fallen, unabhaengig von den Eingaben.

use bolero::check;
use sentinel_physics::{calculate_co2, calculate_noise_level};

/// Sanitize f32: NaN/Inf → default.
fn san(v: f32, default: f32) -> f32 {
    if v.is_finite() {
        v
    } else {
        default
    }
}

#[test]
fn noise_db_never_below_base() {
    check!()
        .with_type::<(u8, bool, bool, f32, f32, f32, f32)>()
        .for_each(|(agents, meeting, phone, adj1, adj2, adj3, adj4)| {
            let agent_count = *agents as usize;
            let adjacent: Vec<f32> = [*adj1, *adj2, *adj3, *adj4]
                .iter()
                .map(|n| san(*n, 30.0).abs().min(200.0))
                .collect();

            let noise = calculate_noise_level(agent_count, *meeting, *phone, &adjacent);

            assert!(
                noise >= 30.0,
                "noise_db below BASE_NOISE_DB (30): {} (agents={}, meeting={}, phone={}, adj={:?})",
                noise,
                agent_count,
                meeting,
                phone,
                adjacent
            );
        });
}

#[test]
fn co2_ppm_never_below_atmospheric() {
    check!().with_type::<(f32, u8, f32, f32)>().for_each(
        |(base_ppm, agents, ventilation, hours)| {
            let base = san(*base_ppm, 400.0).abs().clamp(400.0, 2000.0);
            let agent_count = *agents as usize;
            let vent = san(*ventilation, 0.5).clamp(0.0, 1.0);
            let elapsed = san(*hours, 1.0).abs().min(24.0);

            let co2 = calculate_co2(base, agent_count, vent, elapsed);

            assert!(
                co2 >= 400.0,
                "co2_ppm below CO2_BASE_PPM (400): {} (base={}, agents={}, vent={}, hours={})",
                co2,
                base,
                agent_count,
                vent,
                elapsed
            );
        },
    );
}
