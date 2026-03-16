//! Matrix Physics: Akustik, Temperatur, CO2, Geruch, Transit, Chaos.
//!
//! Physikalische Simulation der Buero-Umgebung.
//! Alle Berechnungen sind pro Raum und pro Tick.

pub mod acoustics;
pub mod chaos;
pub mod smell;
pub mod temperature;
pub mod transit;

pub use acoustics::{calculate_noise_level, noise_to_text};
pub use chaos::{
    chaos_frequency_per_hour, chaos_noise_bonus_db, chaos_temperature_delta_celsius,
    default_chaos_duration_ticks, should_trigger_chaos, ChaosEventType,
};
pub use smell::{is_smell_active, smell_intensity_at_distance, SmellEvent, SmellType};
pub use temperature::{calculate_co2, calculate_temperature, co2_to_text};
pub use transit::{check_hallway_encounter, start_transit, tick_transit, TransitState};
