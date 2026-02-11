//! Temperatur- und CO2-Simulation pro Raum

const BODY_HEAT_PER_AGENT: f32 = 0.3; // °C pro Agent
const WINDOW_DELTA_FACTOR: f32 = 0.5; // (outside - base) * factor
const SUN_MAX_BONUS: f32 = 2.0; // Max Temperaturerhoehung durch Sonne
const CO2_BASE_PPM: f32 = 400.0;
const CO2_PER_AGENT_PER_HOUR: f32 = 40.0;

/// Berechnet Raumtemperatur
pub fn calculate_temperature(
    base_temp: f32, // Basis-Raumtemperatur (z.B. 21.0°C)
    agent_count: usize,
    window_open: bool,
    outside_temp: f32,
    sun_exposure: f32, // 0.0-1.0 (Sonneneinwirkung)
) -> f32 {
    let mut temp = base_temp;
    temp += BODY_HEAT_PER_AGENT * agent_count as f32;
    if window_open {
        temp += (outside_temp - base_temp) * WINDOW_DELTA_FACTOR;
    }
    temp += sun_exposure.clamp(0.0, 1.0) * SUN_MAX_BONUS;
    temp
}

/// Berechnet CO2-Konzentration in ppm
pub fn calculate_co2(
    base_ppm: f32, // Basis CO2 (typisch 400)
    agent_count: usize,
    ventilation_rate: f32, // 0.0-1.0 (0=keine, 1=maximale Lueftung)
    elapsed_hours: f32,
) -> f32 {
    let co2_buildup = CO2_PER_AGENT_PER_HOUR * agent_count as f32 * elapsed_hours;
    let ventilation_reduction = co2_buildup * ventilation_rate.clamp(0.0, 1.0);
    (base_ppm + co2_buildup - ventilation_reduction).max(CO2_BASE_PPM)
}

/// Mappt CO2 ppm auf Wahrnehmungstext
pub fn co2_to_text(co2_ppm: f32) -> &'static str {
    match co2_ppm {
        n if n < 600.0 => "Frische Luft",
        n if n < 1000.0 => "Normale Raumluft",
        n if n < 1500.0 => "Es wird stickig",
        _ => "Kopfschmerzen und Schwindel",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_relative_eq;

    #[test]
    fn test_empty_room_base_temperature() {
        let temp = calculate_temperature(21.0, 0, false, 10.0, 0.0);
        assert_relative_eq!(temp, 21.0, epsilon = 1.0);
    }

    #[test]
    fn test_agents_body_heat() {
        let temp = calculate_temperature(21.0, 5, false, 10.0, 0.0);
        // 21 + 5 * 0.3 = 22.5
        assert_relative_eq!(temp, 22.5, epsilon = 1.0);
    }

    #[test]
    fn test_window_open_cold_weather() {
        let temp = calculate_temperature(21.0, 0, true, 10.0, 0.0);
        // 21 + (10 - 21) * 0.5 = 21 - 5.5 = 15.5
        assert_relative_eq!(temp, 15.5, epsilon = 1.0);
    }

    #[test]
    fn test_sun_exposure_max() {
        let temp = calculate_temperature(21.0, 0, false, 10.0, 1.0);
        // 21 + 1.0 * 2.0 = 23.0
        assert_relative_eq!(temp, 23.0, epsilon = 1.0);
    }

    #[test]
    fn test_co2_empty_room() {
        let co2 = calculate_co2(400.0, 0, 0.0, 1.0);
        assert_relative_eq!(co2, 400.0, epsilon = 1.0);
    }

    #[test]
    fn test_co2_buildup_no_ventilation() {
        // 5 agents, 1 hour, no ventilation
        // 400 + 5 * 40 * 1 = 400 + 200 = 600
        let co2 = calculate_co2(400.0, 5, 0.0, 1.0);
        assert_relative_eq!(co2, 600.0, epsilon = 1.0);
    }

    #[test]
    fn test_co2_with_ventilation() {
        // 5 agents, 1 hour, 50% ventilation
        // buildup = 200, reduction = 200 * 0.5 = 100
        // 400 + 200 - 100 = 500
        let co2 = calculate_co2(400.0, 5, 0.5, 1.0);
        assert_relative_eq!(co2, 500.0, epsilon = 1.0);
    }

    #[test]
    fn test_co2_to_text_mapping() {
        assert_eq!(co2_to_text(400.0), "Frische Luft");
        assert_eq!(co2_to_text(599.9), "Frische Luft");
        assert_eq!(co2_to_text(600.0), "Normale Raumluft");
        assert_eq!(co2_to_text(999.9), "Normale Raumluft");
        assert_eq!(co2_to_text(1000.0), "Es wird stickig");
        assert_eq!(co2_to_text(1499.9), "Es wird stickig");
        assert_eq!(co2_to_text(1500.0), "Kopfschmerzen und Schwindel");
        assert_eq!(co2_to_text(2000.0), "Kopfschmerzen und Schwindel");
    }
}
