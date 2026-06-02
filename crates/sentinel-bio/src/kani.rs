use sentinel_common::components::BioState;

use crate::{apply_psi_stress, drink_water, eat_meal, use_bathroom};

fn bounded_0_100() -> f32 {
    let value: f32 = kani::any();
    kani::assume(value >= 0.0);
    kani::assume(value <= 100.0);
    value
}

fn bounded_pressure() -> f64 {
    let value: f64 = kani::any();
    kani::assume(value >= 0.0);
    kani::assume(value <= 1_000.0);
    value
}

fn bounded_bio_state() -> BioState {
    BioState {
        hunger: bounded_0_100(),
        energy: bounded_0_100(),
        caffeine_mg: bounded_0_100(),
        bladder: bounded_0_100(),
        stress: bounded_0_100(),
        social_need: bounded_0_100(),
        comfort: bounded_0_100(),
    }
}

fn assert_core_bounds(bio: &BioState) {
    assert!(bio.hunger >= 0.0 && bio.hunger <= 100.0);
    assert!(bio.energy >= 0.0 && bio.energy <= 100.0);
    assert!(bio.bladder >= 0.0 && bio.bladder <= 100.0);
    assert!(bio.stress >= 0.0 && bio.stress <= 100.0);
    assert!(bio.social_need >= 0.0 && bio.social_need <= 100.0);
    assert!(bio.comfort >= 0.0 && bio.comfort <= 100.0);
}

#[kani::proof]
fn bio_actions_keep_core_fields_bounded() {
    let mut bio = bounded_bio_state();

    eat_meal(&mut bio);
    drink_water(&mut bio);
    use_bathroom(&mut bio);

    assert_core_bounds(&bio);
}

#[kani::proof]
fn psi_stress_keeps_stress_and_comfort_bounded() {
    let mut bio = bounded_bio_state();
    let cpu_avg10 = bounded_pressure();
    let mem_avg10 = bounded_pressure();

    apply_psi_stress(&mut bio, cpu_avg10, mem_avg10);

    assert!(bio.stress >= 0.0 && bio.stress <= 100.0);
    assert!(bio.comfort >= 0.0 && bio.comfort <= 100.0);
}
