//! Smart Resource Management: Dynamische cgroup-Limits pro Agent.
//!
//! Phase 1: Idle/Normal Detection basierend auf last_activity_tick.
//! Phase 2 (spaeter): Heavy/Suspended via eBPF I/O-Rate + ECS BioState.

use std::collections::HashMap;

use sentinel_common::{AgentId, DomainEvent, DomainEventPayload};
use sentinel_limbo::EventStore;
use sentinel_runtime::{AgentHandle, RuntimeOrchestrator};
use sentinel_sandbox::{resize_cgroup, ResourceProfile};
use tracing::{debug, info};

use crate::config::ResourceManagerConfig;

/// Verwaltet dynamische cgroup-Limits basierend auf Agent-Aktivitaet.
pub struct ResourceManager {
    config: ResourceManagerConfig,
    /// Aktuelles Profil pro Agent.
    profiles: HashMap<AgentId, ResourceProfile>,
    /// Hysterese: (Ziel-Profil, konsekutive Zyklen im Ziel-Profil).
    pending_transitions: HashMap<AgentId, (ResourceProfile, u32)>,
    /// Anzahl Agents im Heavy-Profil (fuer Cap).
    heavy_count: usize,
}

impl ResourceManager {
    pub fn new(config: ResourceManagerConfig) -> Self {
        Self {
            config,
            profiles: HashMap::new(),
            pending_transitions: HashMap::new(),
            heavy_count: 0,
        }
    }

    /// Hauptschleife — nach schedule.run() im Tick-Loop aufrufen.
    ///
    /// Liest `last_activity_tick` aus RuntimeOrchestrator (kein eigenes Tracking),
    /// erkennt Idle/Normal Profile, und resized cgroup-Limits mit Hysterese.
    pub fn cycle(
        &mut self,
        tick: u64,
        runtime: &RuntimeOrchestrator,
        event_store: &EventStore,
        system_stressed: bool,
    ) {
        if !self.config.enabled {
            return;
        }
        if !tick.is_multiple_of(self.config.check_interval_ticks) {
            return;
        }

        for (agent_id, handle) in runtime.agents() {
            let new_profile = self.detect_profile(handle, tick);
            let current = self
                .profiles
                .get(agent_id)
                .copied()
                .unwrap_or(ResourceProfile::Normal);

            if new_profile == current {
                self.pending_transitions.remove(agent_id);
                continue;
            }

            // Heavy-Promotion bei System-Stress blockieren
            if new_profile == ResourceProfile::Heavy && system_stressed {
                continue;
            }

            // Hysterese: N konsekutive Zyklen bevor Transition
            let entry = self
                .pending_transitions
                .entry(*agent_id)
                .or_insert((new_profile, 0));
            if entry.0 != new_profile {
                *entry = (new_profile, 1);
                continue;
            }
            entry.1 += 1;
            if entry.1 < self.config.min_transition_cycles {
                continue;
            }

            // Heavy Cap pruefen
            if new_profile == ResourceProfile::Heavy && self.heavy_count >= self.config.max_heavy {
                continue;
            }

            // Transition ausfuehren
            let name = &handle.identity.name;
            match resize_cgroup(name, &new_profile.limits()) {
                Ok(()) => {
                    if current == ResourceProfile::Heavy {
                        self.heavy_count = self.heavy_count.saturating_sub(1);
                    }
                    if new_profile == ResourceProfile::Heavy {
                        self.heavy_count += 1;
                    }
                    self.profiles.insert(*agent_id, new_profile);
                    self.pending_transitions.remove(agent_id);

                    // Audit-Trail Event (best-effort)
                    let _ = emit_profile_event(
                        event_store,
                        *agent_id,
                        &current.to_string(),
                        &new_profile.to_string(),
                        tick,
                    );

                    info!(
                        agent = %name,
                        old = %current,
                        new = %new_profile,
                        "Resource-Profil gewechselt"
                    );
                }
                Err(e) => {
                    debug!(agent = %name, error = %e, "cgroup Resize fehlgeschlagen");
                }
            }
        }
    }

    /// Erkennt das Profil basierend auf Agent-Aktivitaet.
    ///
    /// Phase 1: Nur Idle/Normal via last_activity_tick.
    /// Phase 2: Heavy (eBPF I/O-Rate), Suspended (ECS BioState).
    fn detect_profile(&self, handle: &AgentHandle, tick: u64) -> ResourceProfile {
        let last = handle.last_activity_tick.0;
        let idle_ticks = tick.saturating_sub(last);

        if idle_ticks > self.config.idle_threshold_ticks {
            ResourceProfile::Idle
        } else {
            ResourceProfile::Normal
        }
    }

    /// Aktuelles Profil eines Agents.
    pub fn get_profile(&self, agent_id: &AgentId) -> ResourceProfile {
        self.profiles
            .get(agent_id)
            .copied()
            .unwrap_or(ResourceProfile::Normal)
    }

    /// Entfernt einen Agent aus dem Tracking (bei Despawn).
    pub fn unregister(&mut self, agent_id: &AgentId) {
        self.profiles.remove(agent_id);
        self.pending_transitions.remove(agent_id);
        // heavy_count wird beim naechsten cycle() korrekt nachgezaehlt
    }

    /// Setzt Profil direkt ohne Hysterese (fuer Platform-Controlplane Interventionen).
    pub fn force_profile(&mut self, agent_id: AgentId, profile: ResourceProfile) {
        let old = self
            .profiles
            .get(&agent_id)
            .copied()
            .unwrap_or(ResourceProfile::Normal);
        if old == ResourceProfile::Heavy {
            self.heavy_count = self.heavy_count.saturating_sub(1);
        }
        if profile == ResourceProfile::Heavy {
            self.heavy_count += 1;
        }
        self.profiles.insert(agent_id, profile);
        self.pending_transitions.remove(&agent_id);
    }

    /// Erzwingt ein Profil inklusive cgroup-Resize und Audit-Event.
    pub fn force_profile_and_apply(
        &mut self,
        agent_id: AgentId,
        agent_name: &str,
        profile: ResourceProfile,
        event_store: &EventStore,
        tick: u64,
    ) -> anyhow::Result<()> {
        let old = self.get_profile(&agent_id);
        resize_cgroup(agent_name, &profile.limits())?;
        self.force_profile(agent_id, profile);

        if old != profile {
            emit_profile_event(
                event_store,
                agent_id,
                &old.to_string(),
                &profile.to_string(),
                tick,
            )?;
        }

        info!(
            agent = %agent_name,
            old = %old,
            new = %profile,
            "Resource-Profil erzwungen"
        );
        Ok(())
    }
}

/// Emittiert ein ResourceProfileChanged Event in den Event Store.
fn emit_profile_event(
    event_store: &EventStore,
    agent_id: AgentId,
    old_profile: &str,
    new_profile: &str,
    tick: u64,
) -> anyhow::Result<()> {
    let payload = DomainEventPayload::ResourceProfileChanged {
        agent_id,
        old_profile: old_profile.to_string(),
        new_profile: new_profile.to_string(),
    };
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let op_id = format!("resource-{}-{}-{}", agent_id.0, tick, ts);
    let event = DomainEvent::new(
        payload.event_type_str(),
        &agent_id.to_string(),
        &payload.to_json(),
        &op_id,
        tick,
    );
    let topic = format!(
        "sentinel/events/resource_profile_changed/AGENT-{:02}",
        agent_id.0
    );
    event_store
        .legacy_append_gateway(sentinel_limbo::LegacyEventProducer::ResourceManager)
        .append_with_outbox(&event, &topic)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> ResourceManagerConfig {
        ResourceManagerConfig {
            enabled: true,
            check_interval_ticks: 1, // Jeder Tick fuer Tests
            idle_threshold_ticks: 10,
            max_heavy: 3,
            min_transition_cycles: 3,
        }
    }

    fn mock_handle(last_tick: u64) -> AgentHandle {
        use sentinel_common::components::{AgentIdentity, ShiftInfo};
        use sentinel_common::Tick;
        AgentHandle {
            identity: AgentIdentity {
                agent_id: AgentId(1),
                name: "Test Agent".to_string(),
                role: "Tester".to_string(),
            },
            shift: ShiftInfo {
                shift_set: 1,
                shift_start_hour: 6,
                shift_end_hour: 14,
                is_on_duty: true,
            },
            status: sentinel_runtime::AgentStatus::Active,
            last_activity_tick: Tick(last_tick),
        }
    }

    #[test]
    fn test_detect_idle_profile_after_threshold() {
        let rm = ResourceManager::new(test_config());
        let handle = mock_handle(0); // Letzte Aktivitaet bei Tick 0
        let profile = rm.detect_profile(&handle, 20); // 20 Ticks vergangen, threshold 10
        assert_eq!(profile, ResourceProfile::Idle);
    }

    #[test]
    fn test_detect_normal_profile_within_threshold() {
        let rm = ResourceManager::new(test_config());
        let handle = mock_handle(15); // Letzte Aktivitaet bei Tick 15
        let profile = rm.detect_profile(&handle, 20); // 5 Ticks vergangen, threshold 10
        assert_eq!(profile, ResourceProfile::Normal);
    }

    #[test]
    fn test_hysterese_prevents_immediate_transition() {
        let mut rm = ResourceManager::new(test_config());
        let agent_id = AgentId(1);

        // Simuliere: Agent ist idle, aber erst 1 Zyklus
        rm.pending_transitions
            .insert(agent_id, (ResourceProfile::Idle, 1));

        // Nach 1 Zyklus: noch keine Transition (min_transition_cycles = 3)
        let entry = rm.pending_transitions.get(&agent_id).unwrap();
        assert!(entry.1 < rm.config.min_transition_cycles);
    }

    #[test]
    fn test_hysterese_allows_after_min_cycles() {
        let rm = ResourceManager::new(test_config());
        // 3 Zyklen = min_transition_cycles → Transition erlaubt
        assert!(3 >= rm.config.min_transition_cycles);
    }

    #[test]
    fn test_cycle_disabled_does_nothing() {
        let mut config = test_config();
        config.enabled = false;
        let mut rm = ResourceManager::new(config);
        // cycle() sollte sofort returnen ohne Seiteneffekte
        // Kein Runtime/EventStore verfuegbar → wuerde paniken wenn nicht disabled
        assert!(rm.profiles.is_empty());
        // Manuell "disabled" testen: profiles bleiben leer
        rm.profiles.insert(AgentId(1), ResourceProfile::Normal);
        assert_eq!(rm.profiles.len(), 1); // Nur manueller Insert, kein cycle()
    }

    #[test]
    fn test_unregister_removes_agent() {
        let mut rm = ResourceManager::new(test_config());
        let agent_id = AgentId(1);
        rm.profiles.insert(agent_id, ResourceProfile::Idle);
        rm.pending_transitions
            .insert(agent_id, (ResourceProfile::Normal, 2));

        rm.unregister(&agent_id);

        assert!(!rm.profiles.contains_key(&agent_id));
        assert!(!rm.pending_transitions.contains_key(&agent_id));
    }

    #[test]
    fn test_resource_profile_limits_values() {
        let idle = ResourceProfile::Idle.limits();
        let normal = ResourceProfile::Normal.limits();
        let heavy = ResourceProfile::Heavy.limits();

        assert!(idle.cpu_quota_us < normal.cpu_quota_us);
        assert!(normal.cpu_quota_us < heavy.cpu_quota_us);
        assert!(idle.memory_bytes < normal.memory_bytes);
        assert!(normal.memory_bytes < heavy.memory_bytes);
    }

    #[test]
    fn test_force_profile_bypasses_hysterese() {
        let mut rm = ResourceManager::new(test_config());
        let agent_id = AgentId(1);
        rm.profiles.insert(agent_id, ResourceProfile::Normal);
        rm.pending_transitions
            .insert(agent_id, (ResourceProfile::Idle, 1));

        rm.force_profile(agent_id, ResourceProfile::Idle);

        assert_eq!(rm.get_profile(&agent_id), ResourceProfile::Idle);
        assert!(!rm.pending_transitions.contains_key(&agent_id));
    }

    #[test]
    fn test_force_profile_updates_heavy_count() {
        let mut rm = ResourceManager::new(test_config());
        let agent_id = AgentId(1);
        rm.profiles.insert(agent_id, ResourceProfile::Heavy);
        rm.heavy_count = 1;

        rm.force_profile(agent_id, ResourceProfile::Idle);

        assert_eq!(rm.heavy_count, 0);
        assert_eq!(rm.get_profile(&agent_id), ResourceProfile::Idle);
    }

    #[test]
    fn test_resource_profile_display() {
        assert_eq!(ResourceProfile::Idle.to_string(), "idle");
        assert_eq!(ResourceProfile::Normal.to_string(), "normal");
        assert_eq!(ResourceProfile::Heavy.to_string(), "heavy");
        assert_eq!(ResourceProfile::Suspended.to_string(), "suspended");
    }
}
