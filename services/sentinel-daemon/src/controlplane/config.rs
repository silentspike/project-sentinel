//! Controlplane-Konfiguration (TOML).

use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

/// Top-level Wrapper (TOML hat `[controlplane]` Section).
#[derive(Debug, Deserialize)]
pub struct ControlplaneConfigFile {
    pub controlplane: ControlplaneConfig,
}

/// Controlplane-Konfiguration.
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct ControlplaneConfig {
    /// Alle N Ticks laeuft ein Controlplane-Zyklus (default: 10).
    #[serde(default = "default_cycle_interval")]
    pub cycle_interval_ticks: u64,

    /// Guarded Mode: Nur beobachten und loggen, keine Aktionen ausfuehren.
    #[serde(default)]
    pub guarded_mode: bool,

    /// Bio-Schwellenwerte fuer Incident-Erkennung.
    #[serde(default)]
    pub thresholds: ThresholdConfig,

    /// Default-TTL fuer Auto-Actions in Ticks.
    #[serde(default = "default_ttl")]
    pub default_ttl_ticks: u64,

    /// Cooldown zwischen gleichen Action-Typen pro Agent in Ticks.
    #[serde(default = "default_cooldown")]
    pub cooldown_ticks: u64,
}

/// Schwellenwerte fuer Bio-Metrics.
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct ThresholdConfig {
    /// Hunger ueber diesem Wert = Incident.
    #[serde(default = "default_hunger_critical")]
    pub hunger_critical: f32,

    /// Energy unter diesem Wert = Incident.
    #[serde(default = "default_energy_critical")]
    pub energy_critical: f32,

    /// Stress ueber diesem Wert = Incident.
    #[serde(default = "default_stress_critical")]
    pub stress_critical: f32,

    /// Bladder ueber diesem Wert = Incident.
    #[serde(default = "default_bladder_critical")]
    pub bladder_critical: f32,
}

impl Default for ThresholdConfig {
    fn default() -> Self {
        Self {
            hunger_critical: default_hunger_critical(),
            energy_critical: default_energy_critical(),
            stress_critical: default_stress_critical(),
            bladder_critical: default_bladder_critical(),
        }
    }
}

fn default_cycle_interval() -> u64 {
    10
}
fn default_ttl() -> u64 {
    30
}
fn default_cooldown() -> u64 {
    60
}
fn default_hunger_critical() -> f32 {
    0.9
}
fn default_energy_critical() -> f32 {
    0.15
}
fn default_stress_critical() -> f32 {
    0.85
}
fn default_bladder_critical() -> f32 {
    0.9
}

impl ControlplaneConfig {
    /// Laedt Config aus einer TOML-Datei.
    pub fn load(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Controlplane-Config lesen: {}", path.display()))?;
        let file: ControlplaneConfigFile = toml::from_str(&content)
            .with_context(|| format!("Controlplane-Config parsen: {}", path.display()))?;
        Ok(file.controlplane)
    }

    /// Erzeugt eine Default-Config (fuer Tests oder wenn keine Datei existiert).
    pub fn default_config() -> Self {
        Self {
            cycle_interval_ticks: default_cycle_interval(),
            guarded_mode: false,
            thresholds: ThresholdConfig::default(),
            default_ttl_ticks: default_ttl(),
            cooldown_ticks: default_cooldown(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_config() {
        let toml_str = r#"
[controlplane]
cycle_interval_ticks = 5
guarded_mode = true
default_ttl_ticks = 20
cooldown_ticks = 30

[controlplane.thresholds]
hunger_critical = 0.85
energy_critical = 0.10
stress_critical = 0.80
bladder_critical = 0.85
"#;
        let file: ControlplaneConfigFile = toml::from_str(toml_str).unwrap();
        let config = file.controlplane;
        assert_eq!(config.cycle_interval_ticks, 5);
        assert!(config.guarded_mode);
        assert_eq!(config.default_ttl_ticks, 20);
        assert!((config.thresholds.hunger_critical - 0.85).abs() < f32::EPSILON);
        assert!((config.thresholds.energy_critical - 0.10).abs() < f32::EPSILON);
    }

    #[test]
    fn test_defaults() {
        let toml_str = r#"
[controlplane]
"#;
        let file: ControlplaneConfigFile = toml::from_str(toml_str).unwrap();
        let config = file.controlplane;
        assert_eq!(config.cycle_interval_ticks, 10);
        assert!(!config.guarded_mode);
        assert_eq!(config.default_ttl_ticks, 30);
        assert!((config.thresholds.hunger_critical - 0.9).abs() < f32::EPSILON);
    }

    #[test]
    fn test_default_config() {
        let config = ControlplaneConfig::default_config();
        assert_eq!(config.cycle_interval_ticks, 10);
        assert!(!config.guarded_mode);
    }
}
