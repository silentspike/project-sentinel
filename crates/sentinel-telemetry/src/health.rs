//! Health check registry for Project Sentinel subsystems.
//!
//! Each subsystem registers a health check function.
//! The registry can be queried for the aggregate health status.

use std::collections::HashMap;
#[cfg(feature = "telemetry")]
use std::sync::OnceLock;
use std::sync::RwLock;

use serde::{Deserialize, Serialize};

// ──────────────────────────────────────────────
// HealthStatus
// ──────────────────────────────────────────────

/// Health status of a subsystem.
///
/// Serialized as part of [`SubsystemMetrics`] for Dashboard transport
/// over Zenoh topic `sentinel/telemetry/health`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum HealthStatus {
    /// Subsystem is operating normally.
    Healthy,
    /// Subsystem is operational but with reduced capability.
    Degraded(String),
    /// Subsystem is not operational.
    Unhealthy(String),
}

// ──────────────────────────────────────────────
// HealthRegistry
// ──────────────────────────────────────────────

/// Global registry for health check functions.
///
/// Subsystems register a closure that returns their current HealthStatus.
/// Access via `HealthRegistry::global()`.
pub struct HealthRegistry {
    checks: RwLock<HashMap<String, Box<dyn Fn() -> HealthStatus + Send + Sync>>>,
}

#[cfg(feature = "telemetry")]
static GLOBAL_HEALTH: OnceLock<HealthRegistry> = OnceLock::new();

impl HealthRegistry {
    /// Get the global health registry (created on first access).
    ///
    /// Only available with the `telemetry` feature (default: enabled).
    #[cfg(feature = "telemetry")]
    pub fn global() -> &'static Self {
        GLOBAL_HEALTH.get_or_init(|| HealthRegistry {
            checks: RwLock::new(HashMap::new()),
        })
    }

    /// Register a health check for a subsystem.
    /// Overwrites any existing check with the same name.
    pub fn register(
        &self,
        name: &str,
        check: impl Fn() -> HealthStatus + Send + Sync + 'static,
    ) {
        let mut checks = self.checks.write().unwrap();
        checks.insert(name.to_string(), Box::new(check));
    }

    /// Run all registered health checks and return results.
    pub fn check_all(&self) -> HashMap<String, HealthStatus> {
        let checks = self.checks.read().unwrap();
        checks
            .iter()
            .map(|(name, check)| (name.clone(), check()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_health_status_variants() {
        let healthy = HealthStatus::Healthy;
        let degraded = HealthStatus::Degraded("slow response".to_string());
        let unhealthy = HealthStatus::Unhealthy("connection lost".to_string());

        assert_eq!(healthy, HealthStatus::Healthy);
        assert_eq!(
            degraded,
            HealthStatus::Degraded("slow response".to_string())
        );
        assert_eq!(
            unhealthy,
            HealthStatus::Unhealthy("connection lost".to_string())
        );
    }

    #[test]
    fn test_health_status_serializable() {
        let status = HealthStatus::Degraded("high latency".to_string());
        let json = serde_json::to_string(&status).unwrap();
        assert!(json.contains("Degraded"));
        assert!(json.contains("high latency"));
    }

    #[test]
    fn test_registry_register_and_check() {
        let registry = HealthRegistry {
            checks: RwLock::new(HashMap::new()),
        };

        registry.register("zenoh", || HealthStatus::Healthy);
        registry.register("redb", || {
            HealthStatus::Degraded("disk slow".to_string())
        });

        let results = registry.check_all();
        assert_eq!(results.len(), 2);
        assert_eq!(results.get("zenoh").unwrap(), &HealthStatus::Healthy);
        assert_eq!(
            results.get("redb").unwrap(),
            &HealthStatus::Degraded("disk slow".to_string())
        );
    }

    #[test]
    fn test_registry_overwrite() {
        let registry = HealthRegistry {
            checks: RwLock::new(HashMap::new()),
        };

        registry.register("db", || HealthStatus::Unhealthy("down".to_string()));
        registry.register("db", || HealthStatus::Healthy);

        let results = registry.check_all();
        assert_eq!(results.get("db").unwrap(), &HealthStatus::Healthy);
    }

    #[test]
    fn test_empty_registry() {
        let registry = HealthRegistry {
            checks: RwLock::new(HashMap::new()),
        };
        let results = registry.check_all();
        assert!(results.is_empty());
    }
}
