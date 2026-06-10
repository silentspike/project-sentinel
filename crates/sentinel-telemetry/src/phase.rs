//! Naming + bucket constants for per-`SimulationPhase` duration histograms (#381).
//!
//! The registry has flat string keys (no labels). Phase histograms use the
//! key scheme `sentinel.ecs.phase.{phase}.duration_ms` so that
//! `export::extract_subsystem` files them under the `ecs` subsystem, while the
//! Prometheus text render maps them back to one metric family with a
//! `phase="..."` label ([`PHASE_DURATION_PROM_NAME`]).

/// Prometheus metric family name for the per-phase duration summary.
pub const PHASE_DURATION_PROM_NAME: &str = "sentinel_phase_duration_ms";

/// Registry key prefix for phase duration histograms.
pub const PHASE_METRIC_PREFIX: &str = "sentinel.ecs.phase.";

/// Registry key suffix for phase duration histograms.
pub const PHASE_METRIC_SUFFIX: &str = ".duration_ms";

/// Default bucket boundaries in milliseconds.
///
/// Phases range from a few microseconds (mood) to ~100 ms (persist under
/// load); the tick budget is 1000 ms. 16 log-spread buckets — winner of the
/// #381 sweep on the deploy VM (i7-3930K): 30.0 ns/observe vs 28.0 ns with
/// 8 buckets, i.e. +2 ns buys double quantile resolution (percentiles are
/// quantized to these boundaries, so resolution dominates the trade-off).
pub const PHASE_DURATION_BOUNDARIES_MS: [f64; 16] = [
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 25.0, 50.0, 100.0, 250.0, 500.0,
];

/// Registry key for one phase, e.g. `sentinel.ecs.phase.biology.duration_ms`.
pub fn phase_metric_name(phase: &str) -> String {
    format!("{PHASE_METRIC_PREFIX}{phase}{PHASE_METRIC_SUFFIX}")
}

/// Inverse of [`phase_metric_name`]: extracts the phase label from a registry
/// key, or `None` if the key does not belong to the phase family.
pub fn phase_label(metric_key: &str) -> Option<&str> {
    metric_key
        .strip_prefix(PHASE_METRIC_PREFIX)?
        .strip_suffix(PHASE_METRIC_SUFFIX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase_metric_name_roundtrip() {
        let key = phase_metric_name("biology");
        assert_eq!(key, "sentinel.ecs.phase.biology.duration_ms");
        assert_eq!(phase_label(&key), Some("biology"));
    }

    #[test]
    fn phase_label_rejects_foreign_keys() {
        assert_eq!(phase_label("sentinel.redb.get.duration_us"), None);
        assert_eq!(phase_label("sentinel.ecs.phase.biology.count"), None);
        assert_eq!(phase_label("sentinel_phase_duration_ms"), None);
    }

    #[test]
    fn boundaries_are_sorted_and_unique() {
        let mut sorted = PHASE_DURATION_BOUNDARIES_MS.to_vec();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
        sorted.dedup();
        assert_eq!(sorted.as_slice(), PHASE_DURATION_BOUNDARIES_MS);
    }
}
