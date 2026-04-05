//! Platform-Controlplane: Self-Healing Background Service.
//!
//! Deterministische Regeln fuer Infrastruktur-Gesundheit:
//! Agent-Stall Detection, Event Store Size, Projection Lag, Memory Pressure.
//!
//! OODA Loop: Observe (metrics) → Decide (rules) → Act (side-effects) → Verify.

#[cfg(feature = "llm")]
pub mod llm_analyzer;
pub mod metrics;
pub mod rules;
pub mod verify;

use anyhow::{anyhow, Context, Result};
use std::collections::{BTreeMap, HashMap};

use sentinel_common::{DomainEvent, DomainEventPayload};
use sentinel_limbo::EventStore;
use serde::{Deserialize, Serialize};
use tracing::info;

use crate::config::PlatformControlplaneConfig;
use metrics::PlatformMetrics;
use rules::{PlatformAction, PlatformSideEffect};

/// Fehlgeschlagene Platform-Intervention fuer LLM-Kontext und Eskalation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailedIntervention {
    pub rule_name: String,
    pub target: String,
    pub action: String,
    pub reason: String,
}

/// Strukturierter Request an den async LLM-Analyzer.
#[derive(Debug, Clone)]
pub struct PlatformAnalysisRequest {
    pub trigger: String,
    pub tick: u64,
    pub metrics: PlatformMetrics,
    pub verify_results: HashMap<String, bool>,
    pub failed_interventions: Vec<FailedIntervention>,
}

/// Strukturierte Analyse, die persistiert und optional ausgefuehrt wird.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PlatformAnalysisCommand {
    pub trigger: String,
    pub severity: String,
    pub summary: String,
    pub recommendation: String,
    #[serde(default)]
    pub suggested_action: Option<String>,
    #[serde(default)]
    pub target: String,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub unresolved_keys: Vec<String>,
    #[serde(default)]
    pub parameters: BTreeMap<String, serde_json::Value>,
}

impl PlatformAnalysisCommand {
    pub fn normalized_target(&self) -> String {
        let target = self.target.trim();
        if target.is_empty() {
            "system".to_string()
        } else {
            target.to_string()
        }
    }
}

/// Deterministischer Test-Hook fuer geplante Triggerpfade.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlatformTriggerTestCommand {
    pub trigger: String,
    #[serde(default)]
    pub rule_name: Option<String>,
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default)]
    pub count: Option<u32>,
}

/// Loopback-Operator-Kommandos fuer Platform-Controlplane.
#[derive(Debug, Clone, PartialEq)]
pub enum PlatformControlCommand {
    AnalyzeNow,
    TriggerTest(PlatformTriggerTestCommand),
    ApplyAnalysis(PlatformAnalysisCommand),
}

/// Read-Only Snapshot fuer Operator- und Dashboard-State.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PlatformStateSnapshot {
    pub current_tick: u64,
    pub stall_recent_activity_grace_ticks: u64,
    pub llm_enabled: bool,
    pub llm_analysis_interval_secs: u64,
    pub llm_retry_delay_secs: u64,
    #[serde(default)]
    pub last_analysis_tick: Option<u64>,
    #[serde(default)]
    pub last_analysis_trigger: Option<String>,
    #[serde(default)]
    pub last_scheduled_analysis_tick: Option<u64>,
    #[serde(default)]
    pub unresolved_counts: BTreeMap<String, u32>,
    #[serde(default)]
    pub threshold_overrides: BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    pub resource_profiles: BTreeMap<String, String>,
    #[serde(default)]
    pub agents: Vec<PlatformAgentSnapshot>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlatformAgentSnapshot {
    pub agent_id: u16,
    pub aggregate_id: String,
    pub name: String,
    pub last_activity_tick: u64,
    pub cgroup_path: String,
    pub current_profile: String,
}

/// Ergebnis eines Platform-Controlplane-Zyklus.
#[derive(Debug, Default)]
pub struct PlatformCycleOutput {
    pub side_effects: Vec<PlatformSideEffect>,
    pub analysis_requests: Vec<PlatformAnalysisRequest>,
}

#[derive(Debug, Clone)]
enum QueuedAnalysisTrigger {
    Manual,
    Test(PlatformTriggerTestCommand),
}

/// Platform-Controlplane mit OODA-Loop und Cooldown-Management.
pub struct PlatformControlplane {
    config: PlatformControlplaneConfig,
    cooldowns: HashMap<String, u64>,
    last_actions: Vec<PlatformAction>,
    unresolved_counts: HashMap<String, u32>,
    failed_interventions: Vec<FailedIntervention>,
    queued_analysis_triggers: Vec<QueuedAnalysisTrigger>,
    last_analysis_tick: Option<u64>,
    last_analysis_trigger: Option<String>,
    last_scheduled_analysis_tick: Option<u64>,
    threshold_overrides: BTreeMap<String, serde_json::Value>,
}

impl PlatformControlplane {
    pub fn new(config: PlatformControlplaneConfig) -> Self {
        Self {
            config,
            cooldowns: HashMap::new(),
            last_actions: Vec::new(),
            unresolved_counts: HashMap::new(),
            failed_interventions: Vec::new(),
            queued_analysis_triggers: Vec::new(),
            last_analysis_tick: None,
            last_analysis_trigger: None,
            last_scheduled_analysis_tick: None,
            threshold_overrides: BTreeMap::new(),
        }
    }

    /// Prueft ob der Zyklus in diesem Tick laufen soll.
    pub fn should_run(&self, tick: u64) -> bool {
        self.config.enabled && tick > 0 && tick.is_multiple_of(self.config.cycle_interval_ticks)
    }

    /// Queue fuer manuelle oder deterministische Test-Trigger.
    pub fn enqueue_control_command(&mut self, command: PlatformControlCommand) {
        match command {
            PlatformControlCommand::AnalyzeNow => {
                self.queued_analysis_triggers.push(QueuedAnalysisTrigger::Manual)
            }
            PlatformControlCommand::TriggerTest(command) => self
                .queued_analysis_triggers
                .push(QueuedAnalysisTrigger::Test(command)),
            PlatformControlCommand::ApplyAnalysis(_) => {}
        }
    }

    pub fn unresolved_counts(&self) -> &HashMap<String, u32> {
        &self.unresolved_counts
    }

    pub fn config(&self) -> &PlatformControlplaneConfig {
        &self.config
    }

    pub fn threshold_overrides(&self) -> &BTreeMap<String, serde_json::Value> {
        &self.threshold_overrides
    }

    pub fn last_analysis_tick(&self) -> Option<u64> {
        self.last_analysis_tick
    }

    pub fn last_analysis_trigger(&self) -> Option<&str> {
        self.last_analysis_trigger.as_deref()
    }

    pub fn last_scheduled_analysis_tick(&self) -> Option<u64> {
        self.last_scheduled_analysis_tick
    }

    pub fn apply_threshold_override(
        &mut self,
        key: &str,
        value: serde_json::Value,
    ) -> Result<()> {
        validate_threshold_override(key, &value)?;
        self.threshold_overrides.insert(key.to_string(), value);
        Ok(())
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
    ) -> PlatformCycleOutput {
        let effective_config = self.effective_config();
        // 1. Verify: Letzte Actions gewirkt?
        let verify_results = verify::verify_last_actions(&self.last_actions, metrics, &effective_config);
        let unresolved_escalations = self.update_unresolved_state(&verify_results);
        self.last_actions.retain(|action| {
            let key = action_key(&action.rule_name, &action.target);
            !verify_results.get(&key).copied().unwrap_or(true)
        });

        let analysis_requests =
            self.build_analysis_requests(metrics, tick, &verify_results, unresolved_escalations);

        // 2. Evaluate: Neue Rules
        let actions = rules::evaluate_rules(
            metrics,
            &self.cooldowns,
            tick,
            &effective_config,
            agent_name_to_id,
        );

        // 3. Execute: Events emittieren + SideEffects sammeln
        let mut side_effects = Vec::new();
        for action in actions {
            let cooldown_key = format!("{}:{}", action.rule_name, action.target);
            self.cooldowns.insert(cooldown_key, tick);

            // PlatformIntervention Event (best-effort)
            let _ = emit_platform_event(event_store, &action, tick);

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

            self.upsert_last_action(action);
        }

        // Cooldown Cleanup: Eintraege aelter als 2x max Cooldown entfernen
        let max_cooldown = self
            .effective_config()
            .prune_cooldown_ticks
            .max(effective_config.stall_cooldown_ticks);
        self.cooldowns
            .retain(|_, &mut last_tick| tick.saturating_sub(last_tick) < max_cooldown * 2);

        PlatformCycleOutput {
            side_effects,
            analysis_requests,
        }
    }

    fn build_analysis_requests(
        &mut self,
        metrics: &PlatformMetrics,
        tick: u64,
        verify_results: &HashMap<String, bool>,
        unresolved_escalations: Vec<String>,
    ) -> Vec<PlatformAnalysisRequest> {
        let mut requests = Vec::new();
        let queued = std::mem::take(&mut self.queued_analysis_triggers);

        if !self.config.llm_enabled {
            return requests;
        }

        if self.scheduled_analysis_due(tick) {
            requests.push(self.make_analysis_request(
                "scheduled",
                tick,
                metrics,
                verify_results.clone(),
                self.failed_interventions.clone(),
            ));
            self.last_scheduled_analysis_tick = Some(tick);
        }

        for key in unresolved_escalations {
            let mut verify = verify_results.clone();
            verify.insert(key.clone(), false);
            let failed = self
                .failed_interventions
                .iter()
                .filter(|item| action_key(&item.rule_name, &item.target) == key)
                .cloned()
                .collect::<Vec<_>>();
            requests.push(self.make_analysis_request(
                "unresolved_escalation",
                tick,
                metrics,
                verify,
                if failed.is_empty() {
                    self.failed_interventions.clone()
                } else {
                    failed
                },
            ));
        }

        for trigger in queued {
            match trigger {
                QueuedAnalysisTrigger::Manual => requests.push(self.make_analysis_request(
                    "manual",
                    tick,
                    metrics,
                    verify_results.clone(),
                    self.failed_interventions.clone(),
                )),
                QueuedAnalysisTrigger::Test(command) => {
                    requests.push(self.make_test_analysis_request(command, tick, metrics, verify_results))
                }
            }
        }

        requests
    }

    fn make_analysis_request(
        &mut self,
        trigger: &str,
        tick: u64,
        metrics: &PlatformMetrics,
        verify_results: HashMap<String, bool>,
        failed_interventions: Vec<FailedIntervention>,
    ) -> PlatformAnalysisRequest {
        self.last_analysis_tick = Some(tick);
        self.last_analysis_trigger = Some(trigger.to_string());
        PlatformAnalysisRequest {
            trigger: trigger.to_string(),
            tick,
            metrics: metrics.clone(),
            verify_results,
            failed_interventions,
        }
    }

    fn make_test_analysis_request(
        &mut self,
        command: PlatformTriggerTestCommand,
        tick: u64,
        metrics: &PlatformMetrics,
        verify_results: &HashMap<String, bool>,
    ) -> PlatformAnalysisRequest {
        let mut effective_results = verify_results.clone();
        let failed_interventions = if command.trigger == "unresolved_escalation" {
            let key = match (&command.rule_name, &command.target) {
                (Some(rule_name), Some(target)) => action_key(rule_name, target),
                _ => "unresolved:test-hook".to_string(),
            };
            effective_results.insert(key.clone(), false);
            vec![FailedIntervention {
                rule_name: command
                    .rule_name
                    .unwrap_or_else(|| "test_hook".to_string()),
                target: command.target.unwrap_or_else(|| "system".to_string()),
                action: "analysis_requested".to_string(),
                reason: format!(
                    "deterministic test trigger (count={})",
                    command.count.unwrap_or(self.config.max_escalation)
                ),
            }]
        } else {
            self.failed_interventions.clone()
        };

        self.make_analysis_request(
            &command.trigger,
            tick,
            metrics,
            effective_results,
            failed_interventions,
        )
    }

    fn scheduled_analysis_due(&self, tick: u64) -> bool {
        let interval = self.config.llm_analysis_interval_secs.max(1);
        match self.last_scheduled_analysis_tick {
            Some(last_tick) => tick.saturating_sub(last_tick) >= interval,
            None => tick >= interval,
        }
    }

    fn update_unresolved_state(&mut self, verify_results: &HashMap<String, bool>) -> Vec<String> {
        let mut escalations = Vec::new();
        let threshold = self.config.max_escalation.max(1);
        let last_actions = self.last_actions.clone();

        for action in &last_actions {
            let key = action_key(&action.rule_name, &action.target);
            match verify_results.get(&key).copied() {
                Some(false) => {
                    let count = self.unresolved_counts.get(&key).copied().unwrap_or(0) + 1;
                    self.unresolved_counts.insert(key.clone(), count);
                    self.record_failed_intervention(action, count);
                    if count == threshold {
                        escalations.push(key);
                    }
                }
                Some(true) | None => {
                    self.unresolved_counts.remove(&key);
                }
            }
        }

        escalations
    }

    fn record_failed_intervention(&mut self, action: &PlatformAction, count: u32) {
        self.failed_interventions.push(FailedIntervention {
            rule_name: action.rule_name.clone(),
            target: action.target.clone(),
            action: action.action_label.clone(),
            reason: format!("unresolved after {count} verification cycle(s)"),
        });

        let max_entries = self.config.llm_max_failed_interventions.max(1);
        if self.failed_interventions.len() > max_entries {
            let excess = self.failed_interventions.len() - max_entries;
            self.failed_interventions.drain(0..excess);
        }
    }

    fn upsert_last_action(&mut self, action: PlatformAction) {
        let key = action_key(&action.rule_name, &action.target);
        if let Some(existing) = self
            .last_actions
            .iter_mut()
            .find(|existing| action_key(&existing.rule_name, &existing.target) == key)
        {
            *existing = action;
        } else {
            self.last_actions.push(action);
        }
    }

    fn effective_config(&self) -> PlatformControlplaneConfig {
        let mut config = self.config.clone();
        if let Some(value) = self.threshold_overrides.get("memory_pressure_threshold") {
            if let Some(parsed) = value.as_f64() {
                config.memory_pressure_threshold = parsed;
            }
        }
        if let Some(value) = self.threshold_overrides.get("max_projection_lag") {
            if let Some(parsed) = value.as_i64() {
                config.max_projection_lag = parsed;
            }
        }
        if let Some(value) = self.threshold_overrides.get("max_event_store_bytes") {
            if let Some(parsed) = value.as_u64() {
                config.max_event_store_bytes = parsed;
            }
        }
        if let Some(value) = self
            .threshold_overrides
            .get("write_anomaly_threshold_bytes_per_sec")
        {
            if let Some(parsed) = value.as_u64() {
                config.write_anomaly_threshold_bytes_per_sec = parsed;
            }
        }
        if let Some(value) = self
            .threshold_overrides
            .get("stall_recent_activity_grace_ticks")
        {
            if let Some(parsed) = value.as_u64() {
                config.stall_recent_activity_grace_ticks = parsed;
            }
        }
        if let Some(value) = self.threshold_overrides.get("stall_cooldown_ticks") {
            if let Some(parsed) = value.as_u64() {
                config.stall_cooldown_ticks = parsed;
            }
        }
        if let Some(value) = self.threshold_overrides.get("prune_cooldown_ticks") {
            if let Some(parsed) = value.as_u64() {
                config.prune_cooldown_ticks = parsed;
            }
        }
        config
    }
}

fn action_key(rule_name: &str, target: &str) -> String {
    format!("{rule_name}:{target}")
}

pub(crate) fn persist_platform_analysis_event(
    event_store: &EventStore,
    tick: u64,
    analysis: &PlatformAnalysisCommand,
) -> Result<()> {
    let target = analysis.normalized_target();
    let payload = DomainEventPayload::PlatformAnalysis {
        trigger: analysis.trigger.clone(),
        severity: analysis.severity.clone(),
        summary: analysis.summary.clone(),
        recommendation: analysis.recommendation.clone(),
        suggested_action: analysis.suggested_action.clone(),
        target: target.clone(),
        provider: analysis.provider.clone(),
        model: analysis.model.clone(),
        unresolved_keys: analysis.unresolved_keys.clone(),
        parameters: analysis.parameters.clone(),
    };
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let op_id = format!("platform-analysis-{}-{tick}-{ts}", analysis.trigger);
    let event = DomainEvent::new(
        payload.event_type_str(),
        &target,
        &payload.to_json(),
        &op_id,
        tick,
    );
    let topic = format!("sentinel/events/platform_analysis/{target}");
    event_store
        .append_with_outbox(&event, &topic)
        .context("persist platform_analysis")?;
    Ok(())
}

fn validate_threshold_override(key: &str, value: &serde_json::Value) -> Result<()> {
    match key {
        "memory_pressure_threshold" => {
            let parsed = value
                .as_f64()
                .context("memory_pressure_threshold muss Zahl sein")?;
            if !(0.0 < parsed && parsed <= 1.0) {
                return Err(anyhow!(
                    "memory_pressure_threshold muss > 0.0 und <= 1.0 sein"
                ));
            }
        }
        "max_projection_lag" => {
            let parsed = value.as_i64().context("max_projection_lag muss Integer sein")?;
            if parsed <= 0 {
                return Err(anyhow!("max_projection_lag muss > 0 sein"));
            }
        }
        "max_event_store_bytes" => {
            let parsed = value
                .as_u64()
                .context("max_event_store_bytes muss Integer sein")?;
            if parsed == 0 {
                return Err(anyhow!("max_event_store_bytes muss > 0 sein"));
            }
        }
        "write_anomaly_threshold_bytes_per_sec" => {
            let parsed = value
                .as_u64()
                .context("write_anomaly_threshold_bytes_per_sec muss Integer sein")?;
            if parsed == 0 {
                return Err(anyhow!(
                    "write_anomaly_threshold_bytes_per_sec muss > 0 sein"
                ));
            }
        }
        "stall_recent_activity_grace_ticks" | "stall_cooldown_ticks" | "prune_cooldown_ticks" => {
            let parsed = value.as_u64().context("Tick-Override muss Integer sein")?;
            if parsed == 0 {
                return Err(anyhow!("{key} muss > 0 sein"));
            }
        }
        _ => return Err(anyhow!("unsupported threshold override key: {key}")),
    }
    Ok(())
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

    #[test]
    fn test_manual_trigger_creates_analysis_request() {
        let mut cp = PlatformControlplane::new(PlatformControlplaneConfig {
            enabled: true,
            cycle_interval_ticks: 1,
            llm_enabled: true,
            llm_analysis_interval_secs: 3600,
            ..PlatformControlplaneConfig::default()
        });
        cp.enqueue_control_command(PlatformControlCommand::AnalyzeNow);

        let dir = tempfile::tempdir().unwrap();
        let db =
            sentinel_limbo::EventStore::open(dir.path().join("manual.db").to_str().unwrap()).unwrap();
        let output = cp.cycle(&PlatformMetrics::default(), &db, 1, &HashMap::new());

        assert_eq!(output.analysis_requests.len(), 1);
        assert_eq!(output.analysis_requests[0].trigger, "manual");
        assert_eq!(cp.last_analysis_trigger(), Some("manual"));
        assert_eq!(cp.last_analysis_tick(), Some(1));
    }

    #[test]
    fn test_scheduled_analysis_respects_interval() {
        let mut cp = PlatformControlplane::new(PlatformControlplaneConfig {
            enabled: true,
            cycle_interval_ticks: 1,
            llm_enabled: true,
            llm_analysis_interval_secs: 5,
            ..PlatformControlplaneConfig::default()
        });
        let dir = tempfile::tempdir().unwrap();
        let db =
            sentinel_limbo::EventStore::open(dir.path().join("sched.db").to_str().unwrap()).unwrap();

        assert!(cp.cycle(&PlatformMetrics::default(), &db, 4, &HashMap::new()).analysis_requests.is_empty());
        let output = cp.cycle(&PlatformMetrics::default(), &db, 5, &HashMap::new());
        assert_eq!(output.analysis_requests.len(), 1);
        assert_eq!(output.analysis_requests[0].trigger, "scheduled");
        assert_eq!(cp.last_scheduled_analysis_tick(), Some(5));
    }

    #[test]
    fn test_unresolved_threshold_triggers_escalation_once() {
        let mut cp = PlatformControlplane::new(PlatformControlplaneConfig {
            enabled: true,
            cycle_interval_ticks: 1,
            llm_enabled: true,
            llm_analysis_interval_secs: 3600,
            stall_cooldown_ticks: 60,
            max_escalation: 3,
            ..PlatformControlplaneConfig::default()
        });
        let dir = tempfile::tempdir().unwrap();
        let db = sentinel_limbo::EventStore::open(dir.path().join("esc.db").to_str().unwrap()).unwrap();
        let metrics = PlatformMetrics {
            failed_services: vec!["sentinel-judge".to_string()],
            ..Default::default()
        };

        let out1 = cp.cycle(&metrics, &db, 1, &HashMap::new());
        assert!(out1.analysis_requests.is_empty());
        let out2 = cp.cycle(&metrics, &db, 2, &HashMap::new());
        assert!(out2.analysis_requests.is_empty());
        let out3 = cp.cycle(&metrics, &db, 3, &HashMap::new());
        assert!(out3.analysis_requests.is_empty());
        let out4 = cp.cycle(&metrics, &db, 4, &HashMap::new());
        assert_eq!(out4.analysis_requests.len(), 1);
        assert_eq!(out4.analysis_requests[0].trigger, "unresolved_escalation");

        let out5 = cp.cycle(&metrics, &db, 5, &HashMap::new());
        assert!(out5.analysis_requests.is_empty());
    }

    #[test]
    fn test_apply_threshold_override_updates_effective_config() {
        let mut cp = PlatformControlplane::new(PlatformControlplaneConfig {
            memory_pressure_threshold: 0.9,
            ..PlatformControlplaneConfig::default()
        });

        cp.apply_threshold_override("memory_pressure_threshold", serde_json::json!(0.75))
            .expect("valid override");

        let metrics = PlatformMetrics {
            agent_memory_pressure: vec![("Test Agent".to_string(), 0.8)],
            ..Default::default()
        };
        let actions = rules::evaluate_rules(
            &metrics,
            &HashMap::new(),
            1,
            &cp.effective_config(),
            &HashMap::new(),
        );

        assert_eq!(
            cp.threshold_overrides()
                .get("memory_pressure_threshold")
                .and_then(|value| value.as_f64()),
            Some(0.75)
        );
        assert!(
            actions
                .iter()
                .any(|action| action.rule_name == "memory_pressure"),
            "override must affect effective rule evaluation"
        );
    }

    #[test]
    fn test_persist_platform_analysis_event_normalizes_empty_target() {
        let dir = tempfile::tempdir().unwrap();
        let db =
            sentinel_limbo::EventStore::open(dir.path().join("analysis.db").to_str().unwrap())
                .unwrap();
        let command = PlatformAnalysisCommand {
            trigger: "operator_test".to_string(),
            severity: "warning".to_string(),
            summary: "Adjust threshold".to_string(),
            recommendation: "Lower memory pressure threshold".to_string(),
            suggested_action: Some("adjust_threshold".to_string()),
            target: String::new(),
            provider: Some("operator-test".to_string()),
            model: Some("manual".to_string()),
            unresolved_keys: vec!["memory_pressure:system".to_string()],
            parameters: BTreeMap::from([
                ("key".to_string(), serde_json::json!("memory_pressure_threshold")),
                ("value".to_string(), serde_json::json!(0.75)),
            ]),
        };

        persist_platform_analysis_event(&db, 7, &command).expect("analysis event persisted");

        let events = db.get_events_since(0, 10).unwrap();
        let event = events
            .iter()
            .find(|event| event.event_type == "platform_analysis")
            .expect("platform_analysis event");
        assert_eq!(event.aggregate_id, "system");
        let payload: DomainEventPayload = serde_json::from_str(&event.payload).unwrap();
        match payload {
            DomainEventPayload::PlatformAnalysis { target, .. } => assert_eq!(target, "system"),
            other => panic!("unexpected payload: {other:?}"),
        }
    }
}
