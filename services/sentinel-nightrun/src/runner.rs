//! Nightrun Runner — Kern-Pipeline fuer Schichtwechsel-Konsolidierung.
//!
//! Sequentielle Verarbeitung: redb ist single-writer, Parallelitaet bringt nichts.
//! Pro Agent wird `HippocampusService::consolidate_agent()` aufgerufen.

use std::path::Path;
use std::time::Instant;

use anyhow::{Context, Result};
use tracing::{error, info, warn};

use sentinel_common::agent_config::load_all_agents;
use sentinel_common::{DomainEvent, DomainEventPayload};
use sentinel_hippocampus::HippocampusService;
use sentinel_limbo::EventStore;

use crate::config::NightrunSettings;
use crate::job_queue::JobQueue;

/// Ergebnis eines Nightrun-Durchlaufs.
#[derive(Debug)]
pub struct NightrunResult {
    pub run_id: String,
    pub agents_consolidated: u32,
    pub agents_failed: u32,
    pub agents_skipped: u32,
    pub total_episodes: u32,
    pub duration_ms: u64,
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
                duration_ms: start.elapsed().as_millis() as u64,
            });
        }

        // 2. Optional shift_set Filter (best-effort, TOML nicht fuer alle vorhanden)
        let agents = self.filter_by_shift(&all_agents, trigger_shift_set);
        let agent_count = agents.len() as u32;

        info!(
            total = all_agents.len(),
            filtered = agents.len(),
            shift = trigger_shift_set,
            "Agent-Selektion abgeschlossen"
        );

        // 3. NightRunStarted Event
        self.emit_started(trigger_shift_set, agent_count)?;

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

        let pending_jobs = self.job_queue.get_pending(&self.run_id)?;
        for job in &pending_jobs {
            // Total-Timeout Check
            if start.elapsed().as_secs() >= self.config.timeout_total_secs {
                warn!(
                    run_id = %self.run_id,
                    elapsed_secs = start.elapsed().as_secs(),
                    "Total-Timeout erreicht, breche ab"
                );
                // Remaining jobs bleiben pending fuer --resume
                break;
            }

            let agent = &job.agent_name;

            // Episode-Count pruefen (Backlog-Limit)
            let episode_count = self.get_episode_count(agent)?;
            if episode_count > self.config.max_episodes_per_agent {
                let reason = format!(
                    "Backlog zu gross: {episode_count} > {}",
                    self.config.max_episodes_per_agent
                );
                warn!(agent, episode_count, max = self.config.max_episodes_per_agent, "Agent uebersprungen");
                self.job_queue.mark_skipped(&self.run_id, agent, &reason)?;
                self.emit_failed(&self.run_id.clone(), agent, &reason)?;
                skipped += 1;
                continue;
            }

            if self.dry_run {
                info!(agent, episode_count, "DRY-RUN: wuerde konsolidieren");
                self.job_queue.mark_skipped(&self.run_id, agent, "dry-run")?;
                skipped += 1;
                continue;
            }

            // Konsolidierung
            self.job_queue
                .mark_in_progress(&self.run_id, agent)?;

            let agent_start = Instant::now();
            match self.consolidate_single_agent(agent) {
                Ok((processed, cons)) => {
                    let duration_ms = agent_start.elapsed().as_millis() as u64;
                    info!(
                        agent,
                        episodes_processed = processed,
                        episodes_consolidated = cons,
                        duration_ms,
                        "Agent konsolidiert"
                    );
                    self.job_queue
                        .mark_completed(&self.run_id, agent, processed, cons)?;
                    self.emit_consolidated(agent, processed, cons, duration_ms)?;
                    consolidated += 1;
                    total_episodes += processed;
                }
                Err(e) => {
                    let err_msg = format!("{e:#}");
                    error!(agent, error = %err_msg, "Konsolidierung fehlgeschlagen");
                    self.job_queue.mark_failed(&self.run_id, agent, &err_msg)?;
                    self.emit_failed(&self.run_id.clone(), agent, &err_msg)?;
                    failed += 1;
                }
            }

            // Per-Agent Timeout Check (warnung, kein Abbruch)
            if agent_start.elapsed().as_secs() >= self.config.timeout_per_agent_secs {
                warn!(
                    agent,
                    elapsed_secs = agent_start.elapsed().as_secs(),
                    "Agent-Timeout ueberschritten"
                );
            }
        }

        let duration_ms = start.elapsed().as_millis() as u64;

        // 6. NightRunCompleted Event
        self.emit_completed(
            trigger_shift_set,
            consolidated,
            failed,
            skipped,
            total_episodes,
            duration_ms,
        )?;

        let result = NightrunResult {
            run_id: self.run_id.clone(),
            agents_consolidated: consolidated,
            agents_failed: failed,
            agents_skipped: skipped,
            total_episodes,
            duration_ms,
        };

        info!(
            run_id = %self.run_id,
            consolidated,
            failed,
            skipped,
            total_episodes,
            duration_ms,
            "Nightrun abgeschlossen"
        );

        Ok(result)
    }

    /// Filtert Agents nach shift_set (best-effort via Agent-TOMLs).
    ///
    /// Agents ohne TOML-Definition werden IMMER inkludiert (konservativ).
    fn filter_by_shift(&self, agents: &[String], trigger_shift_set: u8) -> Vec<String> {
        let agent_configs = load_all_agents(Path::new(&self.config.agent_config_dir))
            .unwrap_or_default();

        // Shift-Set Lookup: name → shift_set
        let shift_map: std::collections::HashMap<String, u8> = agent_configs
            .into_iter()
            .map(|c| (c.identity.name.clone(), c.identity.shift_set))
            .collect();

        agents
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
            .collect()
    }

    /// Konsolidiert einen einzelnen Agent ueber HippocampusService.
    fn consolidate_single_agent(&self, agent: &str) -> Result<(u32, u32)> {
        let result = self
            .hippocampus
            .consolidate_agent(agent)
            .with_context(|| format!("Consolidation failed for agent: {agent}"))?;

        Ok((
            result.episodes_processed as u32,
            result.episodes_consolidated as u32,
        ))
    }

    /// Ermittelt die Episode-Anzahl fuer einen Agent.
    fn get_episode_count(&self, agent: &str) -> Result<usize> {
        let episodes = self.hippocampus.store().load_episodes(agent)?;
        Ok(episodes.len())
    }

    // === Event Emission ===

    fn emit_started(&self, trigger_shift_set: u8, agents_queued: u32) -> Result<()> {
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
        Ok(())
    }

    fn emit_completed(
        &self,
        trigger_shift_set: u8,
        agents_consolidated: u32,
        agents_failed: u32,
        agents_skipped: u32,
        total_episodes: u32,
        duration_ms: u64,
    ) -> Result<()> {
        let payload = DomainEventPayload::NightRunCompleted {
            run_id: self.run_id.clone(),
            trigger_shift_set,
            agents_consolidated,
            agents_failed,
            agents_skipped,
            total_episodes,
            duration_ms,
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
        episodes_processed: u32,
        episodes_consolidated: u32,
        duration_ms: u64,
    ) -> Result<()> {
        let payload = DomainEventPayload::AgentConsolidated {
            run_id: self.run_id.clone(),
            agent_name: agent_name.to_string(),
            episodes_processed,
            episodes_consolidated,
            duration_ms,
        };
        let event = DomainEvent::new(
            payload.event_type_str(),
            agent_name,
            &payload.to_json(),
            &self.run_id,
            0,
        );
        self.event_store
            .append_with_outbox(&event, "nightrun")
            .context("Failed to emit AgentConsolidated")?;
        Ok(())
    }

    fn emit_failed(&self, run_id: &str, agent_name: &str, error: &str) -> Result<()> {
        let payload = DomainEventPayload::AgentConsolidationFailed {
            run_id: run_id.to_string(),
            agent_name: agent_name.to_string(),
            error: error.to_string(),
        };
        let event = DomainEvent::new(
            payload.event_type_str(),
            agent_name,
            &payload.to_json(),
            &self.run_id,
            0,
        );
        self.event_store
            .append_with_outbox(&event, "nightrun")
            .context("Failed to emit AgentConsolidationFailed")?;
        Ok(())
    }
}
