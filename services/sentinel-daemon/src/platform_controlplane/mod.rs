//! Platform-Controlplane: Self-Healing Background Service.
//!
//! Deterministische Regeln fuer Infrastruktur-Gesundheit:
//! Agent-Stall Detection, Event Store Size, Projection Lag, Memory Pressure.
//!
//! OODA Loop: Observe (metrics) → Decide (rules) → Act (side-effects) → Verify.

pub mod metrics;
pub mod rules;
pub mod verify;

use std::collections::HashMap;

use sentinel_common::{DomainEvent, DomainEventPayload};
use sentinel_limbo::EventStore;
use tracing::info;

use crate::config::PlatformControlplaneConfig;
use metrics::PlatformMetrics;
use rules::{PlatformAction, PlatformSideEffect};

/// Platform-Controlplane mit OODA-Loop und Cooldown-Management.
pub struct PlatformControlplane {
    config: PlatformControlplaneConfig,
    cooldowns: HashMap<String, u64>,
    last_actions: Vec<PlatformAction>,
}

impl PlatformControlplane {
    pub fn new(config: PlatformControlplaneConfig) -> Self {
        Self {
            config,
            cooldowns: HashMap::new(),
            last_actions: Vec::new(),
        }
    }

    /// Prueft ob der Zyklus in diesem Tick laufen soll.
    pub fn should_run(&self, tick: u64) -> bool {
        self.config.enabled && tick > 0 && tick.is_multiple_of(self.config.cycle_interval_ticks)
    }

    /// Fuehrt einen OODA-Zyklus aus und gibt Side-Effects zurueck.
    ///
    /// Der Orchestrator fuehrt die Side-Effects aus (TriggerPrune, ForceIdleProfile).
    pub fn cycle(
        &mut self,
        metrics: &PlatformMetrics,
        event_store: &EventStore,
        tick: u64,
        agent_name_to_id: &std::collections::HashMap<String, sentinel_common::AgentId>,
    ) -> Vec<PlatformSideEffect> {
        // 1. Verify: Letzte Actions gewirkt?
        let _verify_results =
            verify::verify_last_actions(&self.last_actions, metrics, &self.config);

        // 2. Evaluate: Neue Rules
        let actions =
            rules::evaluate_rules(metrics, &self.cooldowns, tick, &self.config, agent_name_to_id);

        // 3. Execute: Events emittieren + SideEffects sammeln
        let mut side_effects = Vec::new();
        for action in &actions {
            let cooldown_key = format!("{}:{}", action.rule_name, action.target);
            self.cooldowns.insert(cooldown_key, tick);

            // PlatformIntervention Event (best-effort)
            let _ = emit_platform_event(event_store, action, tick);

            info!(
                rule = %action.rule_name,
                target = %action.target,
                action = %action.action_label,
                "Platform-Intervention"
            );

            // Side-Effect fuer Orchestrator
            if let Some(effect) = &action.side_effect {
                side_effects.push(effect.clone());
            }
        }

        self.last_actions = actions;

        // Cooldown Cleanup: Eintraege aelter als 2x max Cooldown entfernen
        let max_cooldown = self
            .config
            .prune_cooldown_ticks
            .max(self.config.stall_cooldown_ticks);
        self.cooldowns
            .retain(|_, &mut last_tick| tick.saturating_sub(last_tick) < max_cooldown * 2);

        side_effects
    }
}

/// Emittiert ein PlatformIntervention Event in den Event Store.
fn emit_platform_event(
    event_store: &EventStore,
    action: &PlatformAction,
    tick: u64,
) -> anyhow::Result<()> {
    let payload = DomainEventPayload::PlatformIntervention {
        rule_name: action.rule_name.clone(),
        target: action.target.clone(),
        action: action.action_label.clone(),
        description: action.description.clone(),
    };
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let op_id = format!("platform-{}-{}-{}", action.rule_name, tick, ts);
    let event = DomainEvent::new(
        payload.event_type_str(),
        &action.target,
        &payload.to_json(),
        &op_id,
        tick,
    );
    let topic = format!("sentinel/events/platform_intervention/{}", action.target);
    event_store.append_with_outbox(&event, &topic)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cycle_disabled_does_nothing() {
        let config = PlatformControlplaneConfig {
            enabled: false,
            ..PlatformControlplaneConfig::default()
        };
        let cp = PlatformControlplane::new(config);
        assert!(!cp.should_run(60));
        assert!(!cp.should_run(120));
    }

    #[test]
    fn test_should_run_respects_interval() {
        let config = PlatformControlplaneConfig {
            enabled: true,
            cycle_interval_ticks: 60,
            ..PlatformControlplaneConfig::default()
        };
        let cp = PlatformControlplane::new(config);
        assert!(!cp.should_run(0)); // Tick 0 = nie
        assert!(!cp.should_run(30)); // Nicht Vielfaches von 60
        assert!(cp.should_run(60)); // Tick 60 = ja
        assert!(cp.should_run(120)); // Tick 120 = ja
    }

    #[test]
    fn test_cooldown_cleanup() {
        let config = PlatformControlplaneConfig {
            enabled: true,
            stall_cooldown_ticks: 60,
            prune_cooldown_ticks: 100,
            ..PlatformControlplaneConfig::default()
        };
        let mut cp = PlatformControlplane::new(config);
        cp.cooldowns.insert("old:entry".to_string(), 10);
        cp.cooldowns.insert("recent:entry".to_string(), 190);

        // Tick 200: max_cooldown=100, 2x=200. "old:entry" at 10 → diff=190 >= 200? Nein.
        // Tick 500: "old:entry" at 10 → diff=490 >= 200? Ja → removed.
        let metrics = PlatformMetrics::default();
        let dir = tempfile::tempdir().unwrap();
        let db =
            sentinel_limbo::EventStore::open(dir.path().join("test.db").to_str().unwrap()).unwrap();
        let _ = cp.cycle(&metrics, &db, 500, &std::collections::HashMap::new());
        assert!(!cp.cooldowns.contains_key("old:entry"));
    }
}
