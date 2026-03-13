//! Snapshot-Tests fuer Physics Wahrnehmungs-Mappings (insta).
//!
//! Sichert die exakten deutschen Wahrnehmungstexte fuer Laerm und CO2 ab.
//! Jede Aenderung an den Texten wird durch Snapshot-Diff sichtbar.

use sentinel_physics::{co2_to_text, noise_to_text};

#[test]
fn snapshot_noise_perception_mapping() {
    let mapping = [
        (30.0, noise_to_text(30.0)),
        (34.9, noise_to_text(34.9)),
        (35.0, noise_to_text(35.0)),
        (49.9, noise_to_text(49.9)),
        (50.0, noise_to_text(50.0)),
        (64.9, noise_to_text(64.9)),
        (65.0, noise_to_text(65.0)),
        (79.9, noise_to_text(79.9)),
        (80.0, noise_to_text(80.0)),
        (100.0, noise_to_text(100.0)),
    ];
    let output: String = mapping
        .iter()
        .map(|(db, text)| format!("{:>6.1} dB => {}", db, text))
        .collect::<Vec<_>>()
        .join("\n");
    insta::assert_snapshot!("noise_perception_map", output);
}

#[test]
fn snapshot_co2_perception_mapping() {
    let mapping = [
        (400.0, co2_to_text(400.0)),
        (599.0, co2_to_text(599.0)),
        (600.0, co2_to_text(600.0)),
        (999.0, co2_to_text(999.0)),
        (1000.0, co2_to_text(1000.0)),
        (1499.0, co2_to_text(1499.0)),
        (1500.0, co2_to_text(1500.0)),
        (2000.0, co2_to_text(2000.0)),
    ];
    let output: String = mapping
        .iter()
        .map(|(ppm, text)| format!("{:>7.0} ppm => {}", ppm, text))
        .collect::<Vec<_>>()
        .join("\n");
    insta::assert_snapshot!("co2_perception_map", output);
}
