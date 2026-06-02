//! Nightrun Runner — Kern-Pipeline fuer Schichtwechsel-Konsolidierung.
//!
//! Sequentielle Verarbeitung: redb ist single-writer, Parallelitaet bringt nichts.
//! Pro Agent wird `HippocampusService::consolidate_agent()` aufgerufen.

use std::path::Path;
use std::time::Instant;

use anyhow::{Context, Result};
use serde::Serialize;
use tracing::{error, info, warn};

use sentinel_common::agent_config::load_all_agents_with_validation;
use sentinel_common::{DomainEvent, DomainEventPayload};
use sentinel_hippocampus::{
    HippocampusService, NMDA_CONSOLIDATION_THRESHOLD, NMDA_MAX_CONSOLIDATION_EPISODES,
    NMDA_SELECTION_RATIONALE,
};
use sentinel_limbo::EventStore;

use crate::config::NightrunSettings;
use crate::guardrails::{GuardrailController, GuardrailDecision};
use crate::hash_chain::HashChain;
use crate::job_queue::JobQueue;

/// Ergebnis eines Nightrun-Durchlaufs.
#[derive(Debug, Clone, Serialize)]
pub struct NightrunResult {
    pub run_id: String,
    pub agents_consolidated: u32,
    pub agents_failed: u32,
    pub agents_skipped: u32,
    pub total_episodes: u32,
    pub total_episodes_consolidated: u32,
    pub selection: NightrunSelectionMetrics,
    pub duration_ms: u64,
    /// Final hash of the deterministic event chain (for replay verification).
    pub hash_chain_final: String,
}

/// Aggregated NMDA episode-selection evidence for one Night-Run.
#[derive(Debug, Clone, Serialize)]
pub struct NightrunSelectionMetrics {
    pub episodes_processed: u32,
    pub episodes_consolidated: u32,
    pub selection_rate: f64,
    pub threshold: f64,
    pub max_consolidation_episodes: usize,
    pub rationale: &'static str,
    pub score_min: Option<f64>,
    pub score_avg: Option<f64>,
    pub score_max: Option<f64>,
    pub agents: Vec<AgentSelectionMetrics>,
}

impl NightrunSelectionMetrics {
    pub fn empty() -> Self {
        Self {
            episodes_processed: 0,
            episodes_consolidated: 0,
            selection_rate: 0.0,
            threshold: NMDA_CONSOLIDATION_THRESHOLD,
            max_consolidation_episodes: NMDA_MAX_CONSOLIDATION_EPISODES,
            rationale: NMDA_SELECTION_RATIONALE,
            score_min: None,
            score_avg: None,
            score_max: None,
            agents: Vec::new(),
        }
    }

    fn record_agent(&mut self, agent: AgentSelectionMetrics) {
        self.episodes_processed += agent.episodes_processed;
        self.episodes_consolidated += agent.episodes_consolidated;
        self.agents.push(agent);
        self.recompute();
    }

    fn recompute(&mut self) {
        self.selection_rate = if self.episodes_processed == 0 {
            0.0
        } else {
            self.episodes_consolidated as f64 / self.episodes_processed as f64
        };

        let mut min_score: Option<f64> = None;
        let mut max_score: Option<f64> = None;
        let mut sum = 0.0;
        let mut count = 0u32;

        for agent in &self.agents {
            if let Some(score) = agent.score_min {
                min_score = Some(min_score.map_or(score, |current| current.min(score)));
            }
            if let Some(score) = agent.score_max {
                max_score = Some(max_score.map_or(score, |current| current.max(score)));
            }
            if let Some(avg) = agent.score_avg {
                sum += avg * agent.episodes_processed as f64;
                count += agent.episodes_processed;
            }
        }

        self.score_min = min_score;
        self.score_max = max_score;
        self.score_avg = if count == 0 {
            None
        } else {
            Some(sum / count as f64)
        };
    }
}

/// NMDA selection evidence for one agent.
#[derive(Debug, Clone, Serialize)]
pub struct AgentSelectionMetrics {
    pub agent_name: String,
    pub episodes_processed: u32,
    pub episodes_consolidated: u32,
    pub selection_rate: f64,
    pub score_min: Option<f64>,
    pub score_avg: Option<f64>,
    pub score_max: Option<f64>,
}

impl AgentSelectionMetrics {
    fn new(
        agent_name: &str,
        episodes_processed: u32,
        episodes_consolidated: u32,
        scores: &[f64],
    ) -> Self {
        let selection_rate = if episodes_processed == 0 {
            0.0
        } else {
            episodes_consolidated as f64 / episodes_processed as f64
        };

        let score_min = scores.iter().copied().reduce(f64::min);
        let score_max = scores.iter().copied().reduce(f64::max);
        let score_avg = if scores.is_empty() {
            None
        } else {
            Some(scores.iter().sum::<f64>() / scores.len() as f64)
        };

        Self {
            agent_name: agent_name.to_string(),
            episodes_processed,
            episodes_consolidated,
            selection_rate,
            score_min,
            score_avg,
            score_max,
        }
    }
}

/// Kern-Pipeline fuer Schichtwechsel-Konsolidierung.
pub struct NightrunRunner {
    hippocampus: HippocampusService,
    event_store: EventStore,
    job_queue: JobQueue,
    config: NightrunSettings,
    run_id: String,
    dry_run: bool,
}

impl NightrunRunner {
    pub fn new(
        hippocampus: HippocampusService,
        event_store: EventStore,
        job_queue: JobQueue,
        config: NightrunSettings,
        run_id: String,
        dry_run: bool,
    ) -> Self {
        Self {
            hippocampus,
            event_store,
            job_queue,
            config,
            run_id,
            dry_run,
        }
    }

    /// Fuehrt den kompletten Nightrun-Durchlauf aus.
    ///
    /// 1. Agents mit pending Episodes ermitteln
    /// 2. Optional shift_set Filter
    /// 3. Job-Queue befuellen
    /// 4. Pro Agent sequentiell konsolidieren
    /// 5. Events emittieren
    pub fn run(&self, trigger_shift_set: u8) -> Result<NightrunResult> {
        let start = Instant::now();
        let guardrails = GuardrailController::from_settings(&self.config);
        let mut hash_chain = HashChain::new(&self.run_id, &self.run_id);

        info!(run_id = %self.run_id, shift = trigger_shift_set, "Nightrun gestartet");

        // 1. Agents mit pending Episodes
        let all_agents = self
            .hippocampus
            .store()
            .list_agents_with_episodes()
            .context("Failed to list agents with episodes")?;

        if all_agents.is_empty() {
            info!("Keine Agents mit pending Episodes gefunden");
            return Ok(NightrunResult {
                run_id: self.run_id.clone(),
                agents_consolidated: 0,
                agents_failed: 0,
                agents_skipped: 0,
                total_episodes: 0,
                total_episodes_consolidated: 0,
                selection: NightrunSelectionMetrics::empty(),
                duration_ms: start.elapsed().as_millis() as u64,
                hash_chain_final: hash_chain.current_hash(),
            });
        }

        // 2. Optional shift_set Filter (best-effort, TOML nicht fuer alle vorhanden)
        let (agents, name_to_id) = self.filter_by_shift(&all_agents, trigger_shift_set);
        let agent_count = agents.len() as u32;

        info!(
            total = all_agents.len(),
            filtered = agents.len(),
            shift = trigger_shift_set,
            "Agent-Selektion abgeschlossen"
        );

        // 3. NightRunStarted Event
        let started_event = self.emit_started(trigger_shift_set, agent_count)?;
        hash_chain.extend(&started_event);

        // 4. Job-Queue befuellen (nur bei neuem Run, nicht bei Resume)
        if self.job_queue.get_pending(&self.run_id)?.is_empty() {
            self.job_queue
                .create_run(&self.run_id, &agents)
                .context("Failed to create job queue run")?;
        }

        // 5. Sequentielle Verarbeitung
        let mut consolidated = 0u32;
        let mut failed = 0u32;
        let mut skipped = 0u32;
        let mut total_episodes = 0u32;
        let mut total_episodes_consolidated = 0u32;
        let mut selection = NightrunSelectionMetrics::empty();

        let pending_jobs = self.job_queue.get_pending(&self.run_id)?;

        // Max-Jobs-per-Run Guardrail (Issue #18 AC-3)
        if let GuardrailDecision::Abort { reason } = guardrails.check_job_count(pending_jobs.len())
        {
            warn!(run_id = %self.run_id, jobs = pending_jobs.len(), reason = %reason, "Guardrail: Max Jobs pro Run");
        }

        for job in &pending_jobs {
            // Total-Timeout Check (via GuardrailController)
            if let GuardrailDecision::Abort { reason } =
                guardrails.check_total_timeout(start.elapsed().as_secs())
            {
                warn!(run_id = %self.run_id, reason = %reason, "Guardrail: Abort");
                break;
            }

            let agent = &job.agent_name;
            // AGENT-XX ID fuer aggregate_id (NATS-kompatibel, keine Spaces)
            let agg_id = name_to_id
                .get(agent.as_str())
                .map(String::as_str)
                .unwrap_or("nightrun");

            // Episode-Count pruefen (via GuardrailController)
            let episode_count = self.get_episode_count(agent)?;
            if let GuardrailDecision::Skip { reason } =
                guardrails.check_agent_backlog(episode_count)
            {
                warn!(agent, episode_count, reason = %reason, "Guardrail: Skip");
                self.job_queue.mark_skipped(&self.run_id, agent, &reason)?;
                let ev = self.emit_failed(&self.run_id.clone(), agent, agg_id, &reason)?;
                hash_chain.extend(&ev);
                skipped += 1;
                continue;
            }

            if self.dry_run {
                info!(agent, episode_count, "DRY-RUN: wuerde konsolidieren");
                self.job_queue
                    .mark_skipped(&self.run_id, agent, "dry-run")?;
                skipped += 1;
                continue;
            }

            // Konsolidierung
            self.job_queue.mark_in_progress(&self.run_id, agent)?;

            let agent_start = Instant::now();
            match self.consolidate_single_agent(agent) {
                Ok(result) => {
                    let processed = result.episodes_processed as u32;
                    let cons = result.episodes_consolidated as u32;
                    let agent_selection =
                        AgentSelectionMetrics::new(agent, processed, cons, &result.episode_scores);
                    selection.record_agent(agent_selection);
                    let duration_ms = agent_start.elapsed().as_millis() as u64;
                    info!(
                        agent,
                        episodes_processed = processed,
                        episodes_consolidated = cons,
                        selection_rate = format!("{:.3}", selection.selection_rate),
                        duration_ms,
                        "Agent konsolidiert"
                    );
                    self.job_queue
                        .mark_completed(&self.run_id, agent, processed, cons)?;
                    let ev = self.emit_consolidated(agent, agg_id, processed, cons, duration_ms)?;
                    hash_chain.extend(&ev);
                    consolidated += 1;
                    total_episodes += processed;
                    total_episodes_consolidated += cons;
                }
                Err(e) => {
                    let err_msg = format!("{e:#}");
                    error!(agent, error = %err_msg, "Konsolidierung fehlgeschlagen");
                    self.job_queue.mark_failed(&self.run_id, agent, &err_msg)?;
                    let ev = self.emit_failed(&self.run_id.clone(), agent, agg_id, &err_msg)?;
                    hash_chain.extend(&ev);
                    failed += 1;
                }
            }

            // Per-Agent Timeout Check (warning via GuardrailController)
            if let GuardrailDecision::Skip { reason } =
                guardrails.check_agent_timeout(agent_start.elapsed().as_secs())
            {
                warn!(agent, reason = %reason, "Guardrail: Agent-Timeout warning");
            }
        }

        let duration_ms = start.elapsed().as_millis() as u64;
        let hash_chain_final = hash_chain.current_hash();

        // 6. NightRunCompleted Event (with hash chain)
        self.emit_completed(
            trigger_shift_set,
            consolidated,
            failed,
            skipped,
            total_episodes,
            total_episodes_consolidated,
            duration_ms,
            &hash_chain_final,
            &selection,
        )?;

        let result = NightrunResult {
            run_id: self.run_id.clone(),
            agents_consolidated: consolidated,
            agents_failed: failed,
            agents_skipped: skipped,
            total_episodes,
            total_episodes_consolidated,
            selection,
            duration_ms,
            hash_chain_final,
        };

        info!(
            run_id = %self.run_id,
            consolidated,
            failed,
            skipped,
            total_episodes,
            total_episodes_consolidated,
            selection_rate = format!("{:.3}", result.selection.selection_rate),
            duration_ms,
            hash_chain = %result.hash_chain_final,
            "Nightrun abgeschlossen"
        );

        Ok(result)
    }

    /// Filtert Agents nach shift_set (best-effort via Agent-TOMLs).
    ///
    /// Agents ohne TOML-Definition werden IMMER inkludiert (konservativ).
    /// Gibt zusaetzlich eine name→AGENT-XX Mapping-Map zurueck fuer korrekte aggregate_ids.
    fn filter_by_shift(
        &self,
        agents: &[String],
        trigger_shift_set: u8,
    ) -> (Vec<String>, std::collections::HashMap<String, String>) {
        let agent_configs = match load_all_agents_with_validation(
            Path::new(&self.config.agent_config_dir),
            self.config.agent_config_validation(),
        ) {
            Ok(configs) => configs,
            Err(error) => {
                warn!(
                    error = %error,
                    agent_config_dir = %self.config.agent_config_dir,
                    "Agent-TOMLs fuer Nightrun-Shift-Filter konnten nicht geladen werden; konservativer Fallback inkludiert Agents ohne TOML-Mapping"
                );
                Vec::new()
            }
        };

        // Name → AGENT-XX Mapping (fuer NATS-kompatible aggregate_ids)
        let name_to_id: std::collections::HashMap<String, String> = agent_configs
            .iter()
            .map(|c| {
                (
                    c.identity.name.clone(),
                    format!("AGENT-{:02}", c.identity.id),
                )
            })
            .collect();

        // Shift-Set Lookup: name → shift_set
        let shift_map: std::collections::HashMap<String, u8> = agent_configs
            .into_iter()
            .map(|c| (c.identity.name.clone(), c.identity.shift_set))
            .collect();

        let filtered = agents
            .iter()
            .filter(|name| {
                match shift_map.get(*name) {
                    Some(&0) => {
                        // Schicht 0 (Sonder) wird NIE konsolidiert
                        false
                    }
                    Some(&shift) => shift == trigger_shift_set,
                    None => {
                        // Kein TOML vorhanden → konservativ inkludieren
                        true
                    }
                }
            })
            .cloned()
            .collect();

        (filtered, name_to_id)
    }

    /// Konsolidiert einen einzelnen Agent ueber HippocampusService.
    fn consolidate_single_agent(
        &self,
        agent: &str,
    ) -> Result<sentinel_hippocampus::ConsolidationResult> {
        let result = self
            .hippocampus
            .consolidate_agent(agent)
            .with_context(|| format!("Consolidation failed for agent: {agent}"))?;

        Ok(result)
    }

    /// Ermittelt die Episode-Anzahl fuer einen Agent.
    fn get_episode_count(&self, agent: &str) -> Result<usize> {
        let episodes = self.hippocampus.store().load_episodes(agent)?;
        Ok(episodes.len())
    }

    // === Event Emission ===

    fn emit_started(&self, trigger_shift_set: u8, agents_queued: u32) -> Result<DomainEvent> {
        let payload = DomainEventPayload::NightRunStarted {
            run_id: self.run_id.clone(),
            trigger_shift_set,
            agents_queued,
        };
        let event = DomainEvent::new(
            payload.event_type_str(),
            "nightrun",
            &payload.to_json(),
            &self.run_id,
            0,
        );
        self.event_store
            .append_with_outbox(&event, "nightrun")
            .context("Failed to emit NightRunStarted")?;
        Ok(event)
    }

    fn emit_completed(
        &self,
        trigger_shift_set: u8,
        agents_consolidated: u32,
        agents_failed: u32,
        agents_skipped: u32,
        total_episodes: u32,
        total_episodes_consolidated: u32,
        duration_ms: u64,
        hash_chain_final: &str,
        selection: &NightrunSelectionMetrics,
    ) -> Result<()> {
        let payload = DomainEventPayload::NightRunCompleted {
            run_id: self.run_id.clone(),
            trigger_shift_set,
            agents_consolidated,
            agents_failed,
            agents_skipped,
            total_episodes,
            total_episodes_consolidated,
            nmda_selection_rate: Some(selection.selection_rate),
            nmda_threshold: Some(selection.threshold),
            nmda_max_consolidation_episodes: Some(selection.max_consolidation_episodes as u32),
            nmda_score_min: selection.score_min,
            nmda_score_avg: selection.score_avg,
            nmda_score_max: selection.score_max,
            duration_ms,
            hash_chain: Some(hash_chain_final.to_string()),
        };
        let event = DomainEvent::new(
            payload.event_type_str(),
            "nightrun",
            &payload.to_json(),
            &self.run_id,
            0,
        );
        self.event_store
            .append_with_outbox(&event, "nightrun")
            .context("Failed to emit NightRunCompleted")?;
        Ok(())
    }

    fn emit_consolidated(
        &self,
        agent_name: &str,
        aggregate_id: &str,
        episodes_processed: u32,
        episodes_consolidated: u32,
        duration_ms: u64,
    ) -> Result<DomainEvent> {
        let payload = DomainEventPayload::AgentConsolidated {
            run_id: self.run_id.clone(),
            agent_name: agent_name.to_string(),
            episodes_processed,
            episodes_consolidated,
            duration_ms,
        };
        let event = DomainEvent::new(
            payload.event_type_str(),
            aggregate_id,
            &payload.to_json(),
            &self.run_id,
            0,
        );
        self.event_store
            .append_with_outbox(&event, "nightrun")
            .context("Failed to emit AgentConsolidated")?;
        Ok(event)
    }

    fn emit_failed(
        &self,
        run_id: &str,
        agent_name: &str,
        aggregate_id: &str,
        error: &str,
    ) -> Result<DomainEvent> {
        let payload = DomainEventPayload::AgentConsolidationFailed {
            run_id: run_id.to_string(),
            agent_name: agent_name.to_string(),
            error: error.to_string(),
        };
        let event = DomainEvent::new(
            payload.event_type_str(),
            aggregate_id,
            &payload.to_json(),
            &self.run_id,
            0,
        );
        self.event_store
            .append_with_outbox(&event, "nightrun")
            .context("Failed to emit AgentConsolidationFailed")?;
        Ok(event)
    }
}
