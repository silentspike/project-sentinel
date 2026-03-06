//! PSI-basierte adaptive Tick-Rate (TOGAF Adaptive Scheduling).
//!
//! Liest System-Level PSI (Pressure Stall Information) aus `/proc/pressure/`
//! und moduliert die ECS Tick-Rate dynamisch:
//!
//! - CPU avg10 > threshold → Tick-Rate x 0.5 (halbe Frequenz)
//! - Mem avg10 > threshold → Agent-Spawn blockiert
//! - IO avg10 > threshold  → Batching-Window auf 500ms
//!
//! TOGAF-Referenz: Lines 2068-2074 (Adaptive Scheduling / Diegetisches Throttling)

use std::time::Duration;

use sentinel_common::psi::{parse_psi, PsiMetrics};
use serde::Deserialize;
use tracing::{debug, warn};

/// Konfiguration fuer adaptive Tick-Rate aus `[daemon.adaptive]`.
#[derive(Debug, Clone, Deserialize)]
pub struct AdaptiveConfig {
    /// Feature aktiviert (default: true).
    #[serde(default = "default_enabled")]
    pub enabled: bool,

    /// CPU PSI avg10 Schwellwert in % (default: 85.0).
    /// Ueberschreitung → Tick-Rate x 0.5.
    #[serde(default = "default_cpu_threshold")]
    pub cpu_threshold: f64,

    /// Memory PSI avg10 Schwellwert in % (default: 80.0).
    /// Ueberschreitung → Agent-Spawn blockiert.
    #[serde(default = "default_mem_threshold")]
    pub mem_threshold: f64,

    /// IO PSI avg10 Schwellwert in % (default: 70.0).
    /// Ueberschreitung → Batching-Window 500ms.
    #[serde(default = "default_io_threshold")]
    pub io_threshold: f64,

    /// Minimale Tick-Dauer in ms (Floor, default: 2000 = 0.5 Hz).
    #[serde(default = "default_min_tick_rate_ms")]
    pub min_tick_rate_ms: u64,

    /// Alle N Ticks PSI lesen (default: 10).
    #[serde(default = "default_psi_sample_interval")]
    pub psi_sample_interval: u64,
}

fn default_enabled() -> bool {
    true
}
fn default_cpu_threshold() -> f64 {
    85.0
}
fn default_mem_threshold() -> f64 {
    80.0
}
fn default_io_threshold() -> f64 {
    70.0
}
fn default_min_tick_rate_ms() -> u64 {
    2000
}
fn default_psi_sample_interval() -> u64 {
    10
}

impl Default for AdaptiveConfig {
    fn default() -> Self {
        Self {
            enabled: default_enabled(),
            cpu_threshold: default_cpu_threshold(),
            mem_threshold: default_mem_threshold(),
            io_threshold: default_io_threshold(),
            min_tick_rate_ms: default_min_tick_rate_ms(),
            psi_sample_interval: default_psi_sample_interval(),
        }
    }
}

/// Adaptive Tick-Rate Controller.
///
/// Liest PSI-Werte periodisch und berechnet die effektive Tick-Rate.
#[derive(Debug)]
pub struct AdaptiveTickRate {
    config: AdaptiveConfig,
    /// Gecachte PSI-Werte (aktualisiert alle N Ticks).
    cpu_psi: PsiMetrics,
    mem_psi: PsiMetrics,
    io_psi: PsiMetrics,
    /// Letzter Tick bei dem PSI gelesen wurde.
    last_sample_tick: u64,
    /// Ob PSI-Dateien verfuegbar sind.
    psi_available: bool,
}

impl AdaptiveTickRate {
    /// Erstellt einen neuen Controller mit gegebener Konfiguration.
    pub fn new(config: AdaptiveConfig) -> Self {
        // Probe ob /proc/pressure/ existiert
        let psi_available = std::fs::metadata("/proc/pressure/cpu").is_ok();
        if !psi_available {
            warn!("PSI nicht verfuegbar (/proc/pressure/ nicht lesbar) — Fallback auf statische Tick-Rate");
        }

        Self {
            config,
            cpu_psi: PsiMetrics::default(),
            mem_psi: PsiMetrics::default(),
            io_psi: PsiMetrics::default(),
            last_sample_tick: 0,
            psi_available,
        }
    }

    /// Erstellt einen Controller mit expliziten PSI-Werten (fuer Tests).
    #[cfg(test)]
    pub fn with_psi(config: AdaptiveConfig, cpu: PsiMetrics, mem: PsiMetrics, io: PsiMetrics) -> Self {
        Self {
            config,
            cpu_psi: cpu,
            mem_psi: mem,
            io_psi: io,
            last_sample_tick: 0,
            psi_available: true,
        }
    }

    /// Aktualisiert PSI-Werte wenn das Sample-Intervall erreicht ist.
    ///
    /// Soll jeden Tick aufgerufen werden — liest nur alle N Ticks.
    pub fn update(&mut self, tick: u64) {
        if !self.config.enabled || !self.psi_available {
            return;
        }

        if tick < self.last_sample_tick + self.config.psi_sample_interval {
            return;
        }

        self.last_sample_tick = tick;
        self.sample_psi();
    }

    /// Liest PSI-Werte aus /proc/pressure/.
    fn sample_psi(&mut self) {
        if let Ok(content) = std::fs::read_to_string("/proc/pressure/cpu") {
            if let Ok(psi) = parse_psi(&content) {
                self.cpu_psi = psi;
            }
        }
        if let Ok(content) = std::fs::read_to_string("/proc/pressure/memory") {
            if let Ok(psi) = parse_psi(&content) {
                self.mem_psi = psi;
            }
        }
        if let Ok(content) = std::fs::read_to_string("/proc/pressure/io") {
            if let Ok(psi) = parse_psi(&content) {
                self.io_psi = psi;
            }
        }

        debug!(
            cpu_avg10 = format!("{:.1}", self.cpu_psi.avg10),
            mem_avg10 = format!("{:.1}", self.mem_psi.avg10),
            io_avg10 = format!("{:.1}", self.io_psi.avg10),
            "PSI sample"
        );
    }

    /// Berechnet die effektive Tick-Rate basierend auf aktuellen PSI-Werten.
    ///
    /// TOGAF: CPU avg10 > 85% → Tick-Rate x 0.5 (doppelte Sleep-Dauer).
    /// Floor: `min_tick_rate_ms` (default 2000ms = 0.5 Hz).
    pub fn compute_effective_rate(&self, base_rate: Duration) -> Duration {
        if !self.config.enabled || !self.psi_available {
            return base_rate;
        }

        let mut rate = base_rate;

        // CPU-Pressure → halbe Frequenz (= doppelte Tick-Dauer)
        if self.cpu_psi.avg10 > self.config.cpu_threshold {
            rate *= 2;
        }

        // Floor: nie langsamer als min_tick_rate_ms
        let floor = Duration::from_millis(self.config.min_tick_rate_ms);
        if rate > floor {
            rate = floor;
        }

        rate
    }

    /// Ob Agent-Spawn blockiert werden soll (Memory-Pressure).
    ///
    /// TOGAF: Mem PSI avg10 > 80% → Agent-Spawn blockiert.
    pub fn should_block_spawn(&self) -> bool {
        self.config.enabled && self.psi_available && self.mem_psi.avg10 > self.config.mem_threshold
    }

    /// Batching-Window in ms (IO-Pressure).
    ///
    /// TOGAF: IO PSI avg10 > 70% → Batching 500ms.
    /// Sonst: 0 (kein Batching).
    pub fn batching_window_ms(&self) -> u64 {
        if self.config.enabled && self.psi_available && self.io_psi.avg10 > self.config.io_threshold {
            500
        } else {
            0
        }
    }

    /// Aktuelle CPU PSI avg10 (fuer Telemetrie/Dashboard).
    pub fn cpu_avg10(&self) -> f64 {
        self.cpu_psi.avg10
    }

    /// Aktuelle Memory PSI avg10 (fuer Telemetrie/Dashboard).
    pub fn mem_avg10(&self) -> f64 {
        self.mem_psi.avg10
    }

    /// Aktuelle IO PSI avg10 (fuer Telemetrie/Dashboard).
    pub fn io_avg10(&self) -> f64 {
        self.io_psi.avg10
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn psi(avg10: f64) -> PsiMetrics {
        PsiMetrics {
            avg10,
            avg60: 0.0,
            avg300: 0.0,
            total: 0,
        }
    }

    #[test]
    fn test_normal_rate_no_pressure() {
        let at = AdaptiveTickRate::with_psi(
            AdaptiveConfig::default(),
            psi(0.0),
            psi(0.0),
            psi(0.0),
        );
        let base = Duration::from_millis(1000);
        assert_eq!(at.compute_effective_rate(base), base);
    }

    #[test]
    fn test_cpu_above_threshold_doubles_rate() {
        let at = AdaptiveTickRate::with_psi(
            AdaptiveConfig::default(),
            psi(90.0), // > 85%
            psi(0.0),
            psi(0.0),
        );
        let base = Duration::from_millis(1000);
        // CPU > 85% → rate * 2 = 2000ms
        assert_eq!(at.compute_effective_rate(base), Duration::from_millis(2000));
    }

    #[test]
    fn test_cpu_below_threshold_keeps_rate() {
        let at = AdaptiveTickRate::with_psi(
            AdaptiveConfig::default(),
            psi(50.0), // < 85%
            psi(0.0),
            psi(0.0),
        );
        let base = Duration::from_millis(1000);
        assert_eq!(at.compute_effective_rate(base), base);
    }

    #[test]
    fn test_cpu_at_exactly_threshold_keeps_rate() {
        let at = AdaptiveTickRate::with_psi(
            AdaptiveConfig::default(),
            psi(85.0), // == 85%, NOT above
            psi(0.0),
            psi(0.0),
        );
        let base = Duration::from_millis(1000);
        assert_eq!(at.compute_effective_rate(base), base);
    }

    #[test]
    fn test_mem_blocks_spawn() {
        let at = AdaptiveTickRate::with_psi(
            AdaptiveConfig::default(),
            psi(0.0),
            psi(85.0), // > 80%
            psi(0.0),
        );
        assert!(at.should_block_spawn());
    }

    #[test]
    fn test_mem_below_threshold_allows_spawn() {
        let at = AdaptiveTickRate::with_psi(
            AdaptiveConfig::default(),
            psi(0.0),
            psi(50.0), // < 80%
            psi(0.0),
        );
        assert!(!at.should_block_spawn());
    }

    #[test]
    fn test_io_batching_above_threshold() {
        let at = AdaptiveTickRate::with_psi(
            AdaptiveConfig::default(),
            psi(0.0),
            psi(0.0),
            psi(75.0), // > 70%
        );
        assert_eq!(at.batching_window_ms(), 500);
    }

    #[test]
    fn test_io_batching_below_threshold() {
        let at = AdaptiveTickRate::with_psi(
            AdaptiveConfig::default(),
            psi(0.0),
            psi(0.0),
            psi(50.0), // < 70%
        );
        assert_eq!(at.batching_window_ms(), 0);
    }

    #[test]
    fn test_floor_never_exceeded() {
        // Auch bei extremem CPU-Druck: max 2000ms (min_tick_rate_ms)
        let at = AdaptiveTickRate::with_psi(
            AdaptiveConfig::default(),
            psi(100.0), // max pressure
            psi(0.0),
            psi(0.0),
        );
        let base = Duration::from_millis(1000);
        // 1000 * 2 = 2000ms, clamped by floor 2000ms
        assert_eq!(at.compute_effective_rate(base), Duration::from_millis(2000));
    }

    #[test]
    fn test_floor_with_higher_base_rate() {
        // Base rate 1500ms * 2 = 3000ms, aber Floor ist 2000ms
        let at = AdaptiveTickRate::with_psi(
            AdaptiveConfig::default(),
            psi(90.0),
            psi(0.0),
            psi(0.0),
        );
        let base = Duration::from_millis(1500);
        assert_eq!(at.compute_effective_rate(base), Duration::from_millis(2000));
    }

    #[test]
    fn test_disabled_returns_base_rate() {
        let config = AdaptiveConfig {
            enabled: false,
            ..Default::default()
        };
        let at = AdaptiveTickRate::with_psi(config, psi(100.0), psi(100.0), psi(100.0));
        let base = Duration::from_millis(1000);
        assert_eq!(at.compute_effective_rate(base), base);
        assert!(!at.should_block_spawn());
        assert_eq!(at.batching_window_ms(), 0);
    }

    #[test]
    fn test_combined_pressure_all_active() {
        let at = AdaptiveTickRate::with_psi(
            AdaptiveConfig::default(),
            psi(90.0), // CPU > 85%  → rate x2
            psi(85.0), // Mem > 80%  → spawn blocked
            psi(75.0), // IO > 70%   → batching 500ms
        );
        let base = Duration::from_millis(1000);
        assert_eq!(at.compute_effective_rate(base), Duration::from_millis(2000));
        assert!(at.should_block_spawn());
        assert_eq!(at.batching_window_ms(), 500);
    }

    #[test]
    fn test_custom_thresholds() {
        let config = AdaptiveConfig {
            cpu_threshold: 50.0,
            mem_threshold: 30.0,
            io_threshold: 20.0,
            ..Default::default()
        };
        let at = AdaptiveTickRate::with_psi(config, psi(55.0), psi(35.0), psi(25.0));
        let base = Duration::from_millis(1000);
        assert_eq!(at.compute_effective_rate(base), Duration::from_millis(2000));
        assert!(at.should_block_spawn());
        assert_eq!(at.batching_window_ms(), 500);
    }
}
