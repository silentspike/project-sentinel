//! Beispiel: 8-Stunden-Arbeitstag mit biologischen Zyklen simulieren

use sentinel_bio::{drink_coffee, eat_meal, update_bio_state, use_bathroom};
use sentinel_common::components::{BioState, Personality, WorkContext};

fn main() {
    // Initialisierung: Morgen-Person, extrovertiert, niedriger Neurotizismus
    let mut bio = BioState {
        hunger: 0.0,      // Frisch gefruehstueckt
        energy: 90.0,     // Ausgeschlafen
        caffeine_mg: 0.0, // Noch kein Kaffee
        bladder: 0.0,     // Frisch von Toilette
        stress: 0.0,      // Entspannt
        social_need: 30.0,
        comfort: 80.0,
    };

    let personality = Personality {
        openness: 0.7,
        conscientiousness: 0.8,
        extraversion: 0.75, // Extrovertiert
        agreeableness: 0.6,
        neuroticism: 0.2, // Stressresistent
        caffeine_tolerance: 0.5,
        is_morning_person: true,
    };

    let mut work = WorkContext {
        current_task: Some("Email-Review".to_string()),
        in_meeting: false,
        has_deadline: false,
        has_conflict: false,
    };

    println!("=== 8-Stunden-Arbeitstag-Simulation ===\n");

    // 08:00 - Start
    println!("08:00 - Arbeitsstart");
    print_bio_state(&bio, 8.0);

    // 09:30 - Kaffee
    simulate_until(&mut bio, &personality, &work, 1.5, 9.5);
    println!("09:30 - Kaffeepause");
    drink_coffee(&mut bio);
    print_bio_state(&bio, 9.5);

    // 10:00 - Meeting
    simulate_until(&mut bio, &personality, &work, 0.5, 10.0);
    work.in_meeting = true;
    println!("10:00 - Meeting-Start (stressig!)");
    update_bio_state(&mut bio, &personality, &work, 60.0, 10.0);
    print_bio_state(&bio, 10.0);

    // 11:00 - Meeting Ende, Toilettenpause
    simulate_until(&mut bio, &personality, &work, 1.0, 11.0);
    work.in_meeting = false;
    println!("11:00 - Meeting-Ende, Toilettenpause");
    use_bathroom(&mut bio);
    print_bio_state(&bio, 11.0);

    // 12:30 - Mittagessen
    simulate_until(&mut bio, &personality, &work, 1.5, 12.5);
    println!("12:30 - Mittagessen");
    eat_meal(&mut bio);
    print_bio_state(&bio, 12.5);

    // 14:00 - Deadline-Stress
    simulate_until(&mut bio, &personality, &work, 1.5, 14.0);
    work.has_deadline = true;
    println!("14:00 - Deadline in Sicht!");
    update_bio_state(&mut bio, &personality, &work, 60.0, 14.0);
    print_bio_state(&bio, 14.0);

    // 16:00 - Feierabend
    simulate_until(&mut bio, &personality, &work, 2.0, 16.0);
    work.has_deadline = false;
    println!("16:00 - Feierabend!");
    print_bio_state(&bio, 16.0);

    println!("\n=== Fazit ===");
    println!(
        "- Hunger nach 8h: {:.1} (trotz Mittagessen wieder angestiegen)",
        bio.hunger
    );
    println!(
        "- Energie: {:.1} (Kaffee + Essen halfen, aber Stress + Zeit senken)",
        bio.energy
    );
    println!("- Stress: {:.1} (Deadline + Meeting)", bio.stress);
    println!(
        "- Soziales Beduerfnis: {:.1} (Extrovertiert → gestiegen)",
        bio.social_need
    );
}

fn simulate_until(
    bio: &mut BioState,
    personality: &Personality,
    work: &WorkContext,
    hours: f32,
    sim_hour: f32,
) {
    let minutes = (hours * 60.0) as usize;
    for _ in 0..minutes {
        update_bio_state(bio, personality, work, 60.0, sim_hour);
    }
}

fn print_bio_state(bio: &BioState, _hour: f32) {
    println!(
        "  Hunger: {:.1}, Energie: {:.1}, Koffein: {:.0}mg, Blase: {:.1}, Stress: {:.1}, \
         Sozial: {:.1}\n",
        bio.hunger, bio.energy, bio.caffeine_mg, bio.bladder, bio.stress, bio.social_need
    );
}
