//! Periodischer PSI-Metriken Publisher.
//!
//! Liest CPU/Memory PSI-Werte aus Agent-cgroups und publiziert sie
//! auf Zenoh Topics fuer die Bio-Engine.

use std::sync::Arc;
use std::time::Duration;

use sentinel_common::psi::PsiMetrics;
use sentinel_zenoh::topics;
use sentinel_zenoh::SentinelBus;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::{trace, warn};

use crate::cgroups;

/// Kombinierte CPU + Memory PSI-Metriken fuer einen Agenten.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentPsi {
    pub cpu: PsiMetrics,
    pub memory: PsiMetrics,
}

/// Default publish interval in seconds.
const DEFAULT_INTERVAL_SECS: u64 = 5;

/// Startet den PSI-Publisher als async Task.
///
/// Liest alle `interval_secs` Sekunden PSI-Werte fuer jeden aktiven Agenten
/// und publiziert sie auf `sentinel/agent/{name}/psi`.
///
/// Graceful: Wenn eine cgroup nicht existiert (Agent despawned), wird der
/// Agent uebersprungen ohne Panic.
pub async fn run_psi_publisher(
    bus: Arc<SentinelBus>,
    agents: Arc<RwLock<Vec<String>>>,
    interval_secs: Option<u64>,
) {
    let interval = Duration::from_secs(interval_secs.unwrap_or(DEFAULT_INTERVAL_SECS));
    let mut ticker = tokio::time::interval(interval);

    loop {
        ticker.tick().await;

        let agent_names = agents.read().await.clone();

        for name in &agent_names {
            match publish_agent_psi(&bus, name).await {
                Ok(()) => trace!("Published PSI for agent {name}"),
                Err(e) => warn!("Failed to publish PSI for agent {name}: {e}"),
            }
        }
    }
}

/// Liest und publiziert PSI-Metriken fuer einen einzelnen Agenten.
async fn publish_agent_psi(bus: &SentinelBus, name: &str) -> anyhow::Result<()> {
    let cpu = match cgroups::read_psi_from_cgroup(name, "cpu") {
        Ok(m) => m,
        Err(_) => return Ok(()), // cgroup existiert nicht — graceful skip
    };

    let memory = match cgroups::read_psi_from_cgroup(name, "memory") {
        Ok(m) => m,
        Err(_) => return Ok(()),
    };

    let psi = AgentPsi { cpu, memory };
    let payload = serde_json::to_vec(&psi)?;
    let topic = topics::agent_psi(name);

    bus.publish(&topic, &payload).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_psi_serialization_roundtrip() {
        let psi = AgentPsi {
            cpu: PsiMetrics {
                avg10: 1.5,
                avg60: 2.3,
                avg300: 0.1,
                total: 12345,
            },
            memory: PsiMetrics {
                avg10: 5.0,
                avg60: 3.0,
                avg300: 1.0,
                total: 67890,
            },
        };

        let json = serde_json::to_vec(&psi).unwrap();
        let deserialized: AgentPsi = serde_json::from_slice(&json).unwrap();

        assert_eq!(deserialized.cpu.avg10, 1.5);
        assert_eq!(deserialized.cpu.total, 12345);
        assert_eq!(deserialized.memory.avg10, 5.0);
        assert_eq!(deserialized.memory.total, 67890);
    }

    #[test]
    fn agent_psi_default_values() {
        let psi = AgentPsi {
            cpu: PsiMetrics::default(),
            memory: PsiMetrics::default(),
        };

        assert_eq!(psi.cpu.avg10, 0.0);
        assert_eq!(psi.cpu.total, 0);
        assert_eq!(psi.memory.avg10, 0.0);
        assert_eq!(psi.memory.total, 0);
    }
}
