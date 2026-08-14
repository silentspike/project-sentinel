//! Episode Producer — Konvertiert DomainEvents aus Limbo zu Hippocampus-Episoden.
//!
//! Laeuft periodisch im ECS Tick-Loop (alle N Ticks), liest neue Events
//! aus dem Limbo EventStore via Cursor und erzeugt Episode-Objekte fuer
//! den HippocampusService. Nightrun konsolidiert diese spaeter.

use std::collections::HashMap;
use std::sync::{mpsc::SyncSender, Arc, RwLock};

use sentinel_common::events::{DomainEvent, DomainEventPayload};
use sentinel_common::AgentId;
use sentinel_hippocampus::{
    episode_projection_source_cut_coverage, Episode, EpisodeProjectionAdmission,
    EpisodeProjectionAdvance, EpisodeProjectionAgent, EpisodeProjectionApplyOutcome,
    EpisodeProjectionControl, EpisodeProjectionCutoverReceipt,
    EpisodeProjectionGenerationCandidate, EpisodeProjectionGenerationDescriptor,
    EpisodeProjectionGenerationPhase, EpisodeProjectionGenerationStatus,
    EpisodeProjectionGenerationSubject, EpisodeProjectionQuarantine,
    EpisodeProjectionQuarantineReason, EpisodeProjectionReadiness, EpisodeProjectionResolution,
    EpisodeProjectionSourceClassification, EpisodeProjectionSourceCoverageEntry,
    EpisodeProjectionSourceCutEvidence, EpisodeProjectionStartPolicy, EpisodeProjectionSubject,
    EpisodeProjectionWrite, EpisodeSourceReceipt, HippocampusService,
    EPISODE_PROJECTION_MAX_LIVE_EPISODES_PER_SUBJECT, EPISODE_PROJECTION_TICK_DURATION_MILLIS,
    EPISODE_PROJECTION_VERSION,
};
use sentinel_limbo::EventStore;
use sha2::{Digest, Sha256};
use tracing::{debug, info, warn};

/// Intervall in Ticks zwischen Episode-Produktionslaeufen.
/// Bei 1s Tick-Rate = alle 30 Sekunden.
const PRODUCE_INTERVAL_TICKS: u64 = 30;

/// Maximale Anzahl Events pro Batch (verhindert zu grosse Queries).
const BATCH_LIMIT: usize = 500;

/// Anzahl aufeinanderfolgender Laeufe ohne konvertierbare Events,
/// ab der eine Warnung geloggt wird.
const STARVATION_WARN_INTERVAL: u32 = 10;

/// Produziert Episoden aus DomainEvents fuer den HippocampusService.
pub struct EpisodeProducer {
    hippocampus: HippocampusService,
    /// Limbo-interner Cursor (SQLite rowid) — Events nach dieser ID werden verarbeitet.
    last_event_id: i64,
    /// Mapping von AgentId(u16) auf Agent-Name fuer Episode-Erzeugung.
    agent_names: HashMap<u16, String>,
    /// Fail-closed admission and redacted diagnostics shared with daemon edges.
    admission_state: SharedEpisodeProjectionAdmissionState,
    /// Versioned duration bound into every generation replay clock.
    tick_duration_millis: u64,
    /// Zaehler fuer aufeinanderfolgende Laeufe ohne konvertierbare Events (Starvation-Diagnostik).
    empty_runs: u32,
}

/// One-time operator-authenticated authorization for a legacy cutover.
#[derive(Debug, Clone)]
pub struct EpisodeProjectionCutoverAuthorization {
    pub source_row_id: i64,
    pub legacy_state_digest: String,
    pub source_cut_digest: String,
    pub authorization_digest: String,
    pub operator_secret: String,
}

/// Non-secret cutover material that may remain in config after activation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EpisodeProjectionCutoverSeal {
    pub source_row_id: i64,
    pub legacy_state_digest: String,
    pub source_cut_digest: String,
    pub authorization_digest: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct EpisodeProjectionBlockerDiagnostic {
    pub source_row_id: i64,
    pub source_event_id: String,
    pub reason: EpisodeProjectionQuarantineReason,
    pub quarantine_digest: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct EpisodeProjectionAgentDiagnostic {
    pub agent_id: u16,
    pub ready: bool,
    pub frontier_source_row_id: Option<i64>,
    pub lag_rows: Option<i64>,
    pub blockers: Vec<EpisodeProjectionBlockerDiagnostic>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct EpisodeProjectionAdmissionSnapshot {
    pub initialized: bool,
    pub integrity_error: bool,
    pub global_frontier_source_row_id: Option<i64>,
    pub global_blockers: Vec<EpisodeProjectionBlockerDiagnostic>,
    pub agents: Vec<EpisodeProjectionAgentDiagnostic>,
}

impl Default for EpisodeProjectionAdmissionSnapshot {
    fn default() -> Self {
        Self {
            initialized: false,
            integrity_error: true,
            global_frontier_source_row_id: None,
            global_blockers: Vec::new(),
            agents: Vec::new(),
        }
    }
}

impl EpisodeProjectionAdmissionSnapshot {
    pub fn allows_agent(&self, agent_id: AgentId) -> bool {
        self.initialized
            && !self.integrity_error
            && self.global_blockers.is_empty()
            && self
                .agents
                .iter()
                .find(|agent| agent.agent_id == agent_id.0)
                .is_some_and(|agent| agent.ready)
    }
}

pub type SharedEpisodeProjectionAdmissionState = Arc<RwLock<EpisodeProjectionAdmissionSnapshot>>;

#[derive(Debug, Clone, serde::Deserialize)]
pub struct EpisodeProjectionResolveRequest {
    pub source_row_id: i64,
    pub source_event_id: String,
    pub request_digest: String,
    pub quarantine_digest: String,
}

#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
pub struct EpisodeProjectionResolveResponse {
    pub resolved: bool,
    pub duplicate: bool,
    pub source_row_id: i64,
    pub source_event_id: String,
    pub episode_id: u64,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
#[serde(deny_unknown_fields)]
pub enum EpisodeProjectionGenerationRequest {
    Stage {
        expected_active_generation_id: String,
    },
    Validate {
        generation_id: String,
        expected_active_generation_id: String,
    },
    Discard {
        generation_id: String,
        expected_active_generation_id: String,
        expected_candidate_digest: String,
    },
    Activate {
        generation_id: String,
        expected_active_generation_id: String,
        expected_candidate_digest: String,
    },
    Rollback {
        generation_id: String,
        expected_active_generation_id: String,
        expected_candidate_digest: String,
    },
    Status,
}

#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
pub struct EpisodeProjectionGenerationResponse {
    pub operation: String,
    pub generation_id: Option<String>,
    pub candidate_digest: Option<String>,
    pub status: EpisodeProjectionGenerationStatus,
}

pub enum EpisodeProjectionOperatorCommand {
    Resolve {
        request: EpisodeProjectionResolveRequest,
        response_tx: SyncSender<Result<EpisodeProjectionResolveResponse, String>>,
    },
    Generation {
        request: EpisodeProjectionGenerationRequest,
        response_tx: SyncSender<Result<EpisodeProjectionGenerationResponse, String>>,
    },
}

/// Offset-Name fuer die Limbo-Offset-Tabelle (Cursor-Persistierung).
const OFFSET_NAME: &str = "episode_producer";

impl EpisodeProducer {
    /// Erstellt einen neuen EpisodeProducer.
    ///
    /// The first start is explicitly persisted as `Beginning`. The Limbo offset
    /// is only a mirror of the Hippocampus-owned durable cursor.
    pub fn new(
        hippocampus: HippocampusService,
        agents: &[(u16, String)],
        event_store: &EventStore,
    ) -> anyhow::Result<Self> {
        Self::new_with_tick_duration(
            hippocampus,
            agents,
            event_store,
            EPISODE_PROJECTION_TICK_DURATION_MILLIS,
        )
    }

    /// Open with the daemon-configured duration bound into replay generations.
    pub fn new_with_tick_duration(
        hippocampus: HippocampusService,
        agents: &[(u16, String)],
        event_store: &EventStore,
        tick_duration_millis: u64,
    ) -> anyhow::Result<Self> {
        Self::open(
            hippocampus,
            agents,
            event_store,
            None,
            None,
            tick_duration_millis,
        )
    }

    /// Construct with an operator-authenticated, exact-state legacy cutover.
    pub fn new_with_cutover_authorization(
        hippocampus: HippocampusService,
        agents: &[(u16, String)],
        event_store: &EventStore,
        authorization: EpisodeProjectionCutoverAuthorization,
    ) -> anyhow::Result<Self> {
        let seal = EpisodeProjectionCutoverSeal {
            source_row_id: authorization.source_row_id,
            legacy_state_digest: authorization.legacy_state_digest,
            source_cut_digest: authorization.source_cut_digest,
            authorization_digest: authorization.authorization_digest,
        };
        Self::open(
            hippocampus,
            agents,
            event_store,
            Some(&seal),
            Some(authorization.operator_secret.as_str()),
            EPISODE_PROJECTION_TICK_DURATION_MILLIS,
        )
    }

    /// Open with optional non-secret cutover config. The secret is required
    /// only while creating the first durable cutover receipt.
    pub fn new_with_cutover_seal(
        hippocampus: HippocampusService,
        agents: &[(u16, String)],
        event_store: &EventStore,
        seal: EpisodeProjectionCutoverSeal,
        operator_secret: Option<&str>,
    ) -> anyhow::Result<Self> {
        Self::new_with_cutover_seal_and_tick_duration(
            hippocampus,
            agents,
            event_store,
            seal,
            operator_secret,
            EPISODE_PROJECTION_TICK_DURATION_MILLIS,
        )
    }

    /// Open a sealed cutover with the daemon-configured replay tick duration.
    pub fn new_with_cutover_seal_and_tick_duration(
        hippocampus: HippocampusService,
        agents: &[(u16, String)],
        event_store: &EventStore,
        seal: EpisodeProjectionCutoverSeal,
        operator_secret: Option<&str>,
        tick_duration_millis: u64,
    ) -> anyhow::Result<Self> {
        Self::open(
            hippocampus,
            agents,
            event_store,
            Some(&seal),
            operator_secret,
            tick_duration_millis,
        )
    }

    fn open(
        hippocampus: HippocampusService,
        agents: &[(u16, String)],
        event_store: &EventStore,
        cutover: Option<&EpisodeProjectionCutoverSeal>,
        operator_secret: Option<&str>,
        tick_duration_millis: u64,
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(
            tick_duration_millis > 0,
            "episode projection tick duration must be positive"
        );
        let mut producer = Self {
            hippocampus,
            last_event_id: 0,
            agent_names: projection_identity_map(agents)?,
            admission_state: Arc::new(RwLock::new(EpisodeProjectionAdmissionSnapshot::default())),
            tick_duration_millis,
            empty_runs: 0,
        };
        producer.initialize_projection(event_store, cutover, operator_secret)?;
        producer.hydrate_durable_projection_identities()?;
        producer.refresh_admission_state()?;
        Ok(producer)
    }

    fn hydrate_durable_projection_identities(&mut self) -> anyhow::Result<()> {
        let frontiers = self
            .hippocampus
            .store()
            .list_episode_projection_frontiers()?;
        let durable_agents = frontiers
            .iter()
            .map(|frontier| EpisodeProjectionAgent {
                subject: frontier.subject,
                agent_name: frontier.agent_name.clone(),
            })
            .collect::<Vec<_>>();
        self.hippocampus
            .store()
            .validate_episode_projection_agents(&durable_agents)?;

        let mut hydrated = self.agent_names.clone();
        let mut names = hydrated
            .iter()
            .map(|(agent_id, agent_name)| (agent_name.clone(), *agent_id))
            .collect::<HashMap<_, _>>();
        for frontier in frontiers {
            match frontier.subject {
                EpisodeProjectionSubject::Agent { agent_id } => {
                    if let Some(existing_name) = hydrated.get(&agent_id.0) {
                        anyhow::ensure!(
                            existing_name == &frontier.agent_name,
                            "durable episode projection subject {:?} conflicts with current roster",
                            frontier.subject
                        );
                    }
                    if let Some(existing_id) = names.get(&frontier.agent_name) {
                        anyhow::ensure!(
                            *existing_id == agent_id.0,
                            "durable episode projection name {} conflicts with current roster",
                            frontier.agent_name
                        );
                    }
                    hydrated.insert(agent_id.0, frontier.agent_name.clone());
                    names.insert(frontier.agent_name, agent_id.0);
                }
                EpisodeProjectionSubject::Building => {
                    anyhow::ensure!(
                        frontier.agent_name == "_building",
                        "durable Building projection has an invalid storage locator"
                    );
                }
            }
        }
        self.agent_names = hydrated;
        Ok(())
    }

    /// Gibt eine Referenz auf den HippocampusService zurueck.
    pub fn hippocampus(&self) -> &HippocampusService {
        &self.hippocampus
    }

    pub fn admission_state(&self) -> SharedEpisodeProjectionAdmissionState {
        Arc::clone(&self.admission_state)
    }

    /// Typed AgentId-based readiness readback for later orchestrator wiring.
    pub fn episode_projection_readiness(
        &self,
        agent_id: AgentId,
    ) -> anyhow::Result<EpisodeProjectionReadiness> {
        self.hippocampus
            .store()
            .load_episode_projection_readiness(EpisodeProjectionSubject::Agent { agent_id })
    }

    /// Registriert einen neuen Agenten (z.B. bei Schichtwechsel).
    pub fn register_agent(&mut self, id: u16, name: String) -> anyhow::Result<()> {
        let projection_agent = EpisodeProjectionAgent {
            subject: EpisodeProjectionSubject::Agent {
                agent_id: AgentId(id),
            },
            agent_name: name.clone(),
        };
        self.hippocampus
            .store()
            .initialize_episode_projection_agent(&projection_agent)?;
        self.agent_names.insert(id, name);
        self.refresh_admission_state()?;
        Ok(())
    }

    /// Validate a complete staged roster against immutable durable bindings.
    pub fn validate_agent_bindings(&self, agents: &[(u16, String)]) -> anyhow::Result<()> {
        let projection_agents = agents
            .iter()
            .map(|(id, name)| EpisodeProjectionAgent {
                subject: EpisodeProjectionSubject::Agent {
                    agent_id: AgentId(*id),
                },
                agent_name: name.clone(),
            })
            .collect::<Vec<_>>();
        self.hippocampus
            .store()
            .validate_episode_projection_agents(&projection_agents)
    }

    /// Idempotently register a prevalidated roster after the durable decision.
    pub fn register_agents(&mut self, agents: &[(u16, String)]) -> anyhow::Result<()> {
        self.validate_agent_bindings(agents)?;
        for (id, name) in agents {
            if self
                .agent_names
                .get(id)
                .is_some_and(|existing| existing == name)
            {
                continue;
            }
            self.register_agent(*id, name.clone())?;
        }
        Ok(())
    }

    /// Ob dieser Tick ein Produktionslauf sein soll.
    pub fn should_run(&self, tick: u64) -> bool {
        tick > 0 && tick.is_multiple_of(PRODUCE_INTERVAL_TICKS)
    }

    /// Verarbeitet neue Events aus Limbo und erzeugt Episoden.
    ///
    /// Gibt die Anzahl produzierter Episoden zurueck.
    pub fn tick(
        &mut self,
        event_store: &EventStore,
        _current_tick: u64,
        _tick_rate_s: f64,
    ) -> usize {
        let events = match event_store.get_events_since_with_id(self.last_event_id, BATCH_LIMIT) {
            Ok(events) => events,
            Err(e) => {
                warn!(error = %e, "Episode Producer: Limbo-Events lesen fehlgeschlagen");
                return 0;
            }
        };

        if events.is_empty() {
            return 0;
        }
        let mut effect_reference_tick =
            match self.hippocampus.store().load_episode_projection_control() {
                Ok(Some(control)) => control.effect_reference_tick,
                Ok(None) => return 0,
                Err(error) => {
                    warn!(%error, "Episode Producer: effect clock could not be loaded");
                    return 0;
                }
            };

        let mut total = 0;
        let mut agents_with_episodes = std::collections::HashSet::new();

        for (source_row_id, event) in &events {
            effect_reference_tick = effect_reference_tick.max(event.tick);
            let request_digest = source_request_digest(event);

            match self
                .hippocampus
                .store()
                .episode_projection_admission(None, *source_row_id)
            {
                Ok(EpisodeProjectionAdmission::GloballyBlocked(blocker)) => {
                    warn!(
                        source_row_id,
                        blocker_row = blocker.source_row_id,
                        "Episode Producer: global unresolved quarantine fences later work"
                    );
                    break;
                }
                Ok(EpisodeProjectionAdmission::Allowed)
                | Ok(EpisodeProjectionAdmission::SubjectBlocked(_)) => {}
                Err(error) => {
                    warn!(source_row_id, %error, "Episode Producer: quarantine admission read failed");
                    break;
                }
            }

            if !is_episode_event_type(&event.event_type) {
                let advance = EpisodeProjectionAdvance {
                    source_event_id: event.event_id.clone(),
                    source_row_id: *source_row_id,
                    projection_version: EPISODE_PROJECTION_VERSION,
                    request_digest,
                    expected_global_frontier: self.last_event_id,
                    effect_reference_tick,
                };
                match self
                    .hippocampus
                    .store()
                    .advance_episode_projection(&advance)
                {
                    Ok(control) => self.commit_source_cursor(event_store, &control),
                    Err(error) => {
                        warn!(source_row_id, event_id = %event.event_id, %error, "Episode Producer: irrelevantes Event konnte nicht quittiert werden");
                        break;
                    }
                }
                continue;
            }

            if let Some(subject) = projection_subject_from_event(event) {
                match self
                    .hippocampus
                    .store()
                    .episode_projection_admission(Some(subject), *source_row_id)
                {
                    Ok(EpisodeProjectionAdmission::SubjectBlocked(blocker)) => {
                        let blocker_digest = quarantine_record_digest(&blocker);
                        if !self.quarantine_event(
                            event_store,
                            *source_row_id,
                            event,
                            Some(subject),
                            request_digest,
                            effect_reference_tick,
                            EpisodeProjectionQuarantineReason::BlockedByEarlierQuarantine,
                            &blocker_digest,
                        ) {
                            break;
                        }
                        continue;
                    }
                    Ok(EpisodeProjectionAdmission::Allowed) => {}
                    Ok(EpisodeProjectionAdmission::GloballyBlocked(blocker)) => {
                        warn!(
                            source_row_id,
                            blocker_row = blocker.source_row_id,
                            "Episode Producer: global unresolved quarantine fences later work"
                        );
                        break;
                    }
                    Err(error) => {
                        warn!(source_row_id, %error, "Episode Producer: subject admission read failed");
                        break;
                    }
                }
            }

            let payload: DomainEventPayload = match serde_json::from_str(&event.payload) {
                Ok(payload) => payload,
                Err(error) => {
                    if !self.quarantine_event(
                        event_store,
                        *source_row_id,
                        event,
                        projection_subject_from_event(event),
                        request_digest,
                        effect_reference_tick,
                        EpisodeProjectionQuarantineReason::MalformedRelevantPayload,
                        &error.to_string(),
                    ) {
                        break;
                    }
                    continue;
                }
            };

            if payload.event_type_str() != event.event_type {
                if !self.quarantine_event(
                    event_store,
                    *source_row_id,
                    event,
                    episode_subject(&payload),
                    request_digest,
                    effect_reference_tick,
                    EpisodeProjectionQuarantineReason::EventTypeMismatch,
                    &format!(
                        "envelope type {} does not match payload type {}",
                        event.event_type,
                        payload.event_type_str()
                    ),
                ) {
                    break;
                }
                continue;
            }

            if let Err(diagnostic) = validate_episode_payload(&payload) {
                if !self.quarantine_event(
                    event_store,
                    *source_row_id,
                    event,
                    episode_subject(&payload),
                    request_digest,
                    effect_reference_tick,
                    EpisodeProjectionQuarantineReason::MalformedRelevantPayload,
                    diagnostic,
                ) {
                    break;
                }
                continue;
            }

            if let Some(subject) = episode_subject(&payload) {
                match self
                    .hippocampus
                    .store()
                    .episode_projection_admission(Some(subject), *source_row_id)
                {
                    Ok(EpisodeProjectionAdmission::SubjectBlocked(blocker)) => {
                        let blocker_digest = quarantine_record_digest(&blocker);
                        if !self.quarantine_event(
                            event_store,
                            *source_row_id,
                            event,
                            Some(subject),
                            request_digest,
                            effect_reference_tick,
                            EpisodeProjectionQuarantineReason::BlockedByEarlierQuarantine,
                            &blocker_digest,
                        ) {
                            break;
                        }
                        continue;
                    }
                    Ok(EpisodeProjectionAdmission::Allowed) => {}
                    Ok(EpisodeProjectionAdmission::GloballyBlocked(blocker)) => {
                        warn!(
                            source_row_id,
                            blocker_row = blocker.source_row_id,
                            "Episode Producer: global unresolved quarantine fences later work"
                        );
                        break;
                    }
                    Err(error) => {
                        warn!(source_row_id, %error, "Episode Producer: subject admission read failed");
                        break;
                    }
                }
            }

            let Some((projection_subject, stable_agent_name)) = self.episode_agent(&payload) else {
                if !self.quarantine_event(
                    event_store,
                    *source_row_id,
                    event,
                    episode_subject(&payload),
                    request_digest,
                    effect_reference_tick,
                    EpisodeProjectionQuarantineReason::UnknownAgent,
                    "relevant event references an unregistered agent",
                ) {
                    break;
                }
                continue;
            };
            let episode_id = stable_episode_id(
                projection_subject,
                &event.event_id,
                EPISODE_PROJECTION_VERSION,
                &request_digest,
            );
            let Some((agent_name, episode)) = self.event_to_episode(
                &payload,
                episode_id,
                episode_hours_ago(event.tick, effect_reference_tick, self.tick_duration_millis),
            ) else {
                warn!(event_id = %event.event_id, "Episode Producer: validiertes relevantes Event wurde nicht konvertiert");
                break;
            };
            debug_assert_eq!(agent_name, stable_agent_name);

            let input = EpisodeProjectionWrite {
                subject: projection_subject,
                agent_name: agent_name.clone(),
                source_event_id: event.event_id.clone(),
                source_row_id: *source_row_id,
                projection_version: EPISODE_PROJECTION_VERSION,
                request_digest,
                expected_global_frontier: self.last_event_id,
                effect_reference_tick,
                episode,
            };
            match self.hippocampus.store().commit_episode_projection(&input) {
                Ok(EpisodeProjectionApplyOutcome::Applied {
                    control, receipt, ..
                }) => {
                    total += 1;
                    agents_with_episodes.insert(agent_name.clone());
                    self.commit_source_cursor(event_store, &control);
                    debug!(
                        agent = %agent_name,
                        episode_id = receipt.episode_id,
                        source_row_id,
                        "Episode committed"
                    );
                }
                Ok(EpisodeProjectionApplyOutcome::Duplicate { control, .. }) => {
                    self.commit_source_cursor(event_store, &control);
                }
                Err(error) => {
                    warn!(agent = %agent_name, source_row_id, event_id = %event.event_id, %error, "Episode Producer: atomarer Commit fehlgeschlagen");
                    break;
                }
            }
        }

        if let Err(error) = self.refresh_admission_state() {
            warn!(%error, "Episode Producer: admission snapshot failed closed");
        }

        if total > 0 {
            self.empty_runs = 0;
            info!(
                episodes = total,
                agents = agents_with_episodes.len(),
                cursor = self.last_event_id,
                "Episoden produziert"
            );
        } else {
            self.empty_runs += 1;
            if self.empty_runs.is_multiple_of(STARVATION_WARN_INTERVAL) {
                warn!(
                    empty_runs = self.empty_runs,
                    cursor = self.last_event_id,
                    events_checked = events.len(),
                    "Episode Producer: Keine konvertierbaren Events seit {} Laeufen",
                    self.empty_runs
                );
            }
        }

        total
    }

    fn initialize_projection(
        &mut self,
        event_store: &EventStore,
        cutover: Option<&EpisodeProjectionCutoverSeal>,
        operator_secret: Option<&str>,
    ) -> anyhow::Result<()> {
        let mut agents: Vec<EpisodeProjectionAgent> = self
            .agent_names
            .iter()
            .map(|(agent_id, agent_name)| EpisodeProjectionAgent {
                subject: EpisodeProjectionSubject::Agent {
                    agent_id: AgentId(*agent_id),
                },
                agent_name: agent_name.clone(),
            })
            .collect();
        agents.push(EpisodeProjectionAgent {
            subject: EpisodeProjectionSubject::Building,
            agent_name: "_building".to_string(),
        });
        agents.sort_by_key(|agent| match agent.subject {
            EpisodeProjectionSubject::Agent { agent_id } => (0, agent_id.0),
            EpisodeProjectionSubject::Building => (1, 0),
        });
        let existing = self.hippocampus.store().load_episode_projection_control()?;
        let control = match existing {
            Some(control) => {
                if let Some(seal) = cutover {
                    self.validate_persisted_cutover_seal(seal)?;
                }
                self.hippocampus
                    .store()
                    .initialize_episode_projection(&control.start_policy, &agents)?
            }
            None => match cutover {
                Some(seal) => {
                    let operator_secret = operator_secret.ok_or_else(|| {
                        anyhow::anyhow!(
                            "new episode projection cutover requires operator_api.shared_secret"
                        )
                    })?;
                    let authorization = EpisodeProjectionCutoverAuthorization {
                        source_row_id: seal.source_row_id,
                        legacy_state_digest: seal.legacy_state_digest.clone(),
                        source_cut_digest: seal.source_cut_digest.clone(),
                        authorization_digest: seal.authorization_digest.clone(),
                        operator_secret: operator_secret.to_string(),
                    };
                    let receipt =
                        self.validate_cutover_authorization(event_store, authorization)?;
                    self.hippocampus
                        .store()
                        .initialize_episode_projection_cutover(&receipt, &agents)?
                }
                None => self.hippocampus.store().initialize_episode_projection(
                    &EpisodeProjectionStartPolicy::Beginning,
                    &agents,
                )?,
            },
        };
        self.commit_source_cursor_checked(event_store, &control)?;
        Ok(())
    }

    fn validate_persisted_cutover_seal(
        &self,
        seal: &EpisodeProjectionCutoverSeal,
    ) -> anyhow::Result<()> {
        let receipt = self
            .hippocampus
            .store()
            .load_episode_projection_cutover_receipt()?
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "episode projection cutover config conflicts with non-cutover state"
                )
            })?;
        anyhow::ensure!(
            receipt.source_row_id == seal.source_row_id
                && constant_time_eq(
                    receipt.legacy_state_digest.as_bytes(),
                    seal.legacy_state_digest.as_bytes(),
                )
                && constant_time_eq(
                    receipt.source_cut_digest.as_bytes(),
                    seal.source_cut_digest.as_bytes(),
                )
                && constant_time_eq(
                    receipt.authorization_digest.as_bytes(),
                    seal.authorization_digest.as_bytes(),
                ),
            "episode projection persisted cutover seal mismatch"
        );
        Ok(())
    }

    fn validate_cutover_authorization(
        &self,
        event_store: &EventStore,
        authorization: EpisodeProjectionCutoverAuthorization,
    ) -> anyhow::Result<EpisodeProjectionCutoverReceipt> {
        anyhow::ensure!(
            authorization.operator_secret.len() >= 32,
            "episode projection cutover requires an operator secret of at least 32 bytes"
        );
        anyhow::ensure!(
            authorization.source_row_id >= 0
                && is_sha256_hex(&authorization.legacy_state_digest)
                && is_sha256_hex(&authorization.source_cut_digest)
                && is_sha256_hex(&authorization.authorization_digest),
            "episode projection cutover authorization is malformed"
        );
        let legacy_material = self
            .hippocampus
            .store()
            .episode_projection_legacy_state_material()?;
        let legacy_state_digest = format!("{:x}", Sha256::digest(&legacy_material));
        anyhow::ensure!(
            constant_time_eq(
                legacy_state_digest.as_bytes(),
                authorization.legacy_state_digest.as_bytes()
            ),
            "episode projection cutover legacy state digest mismatch"
        );
        let source_cut_digest =
            event_store_source_cut_digest(event_store, authorization.source_row_id)?;
        anyhow::ensure!(
            constant_time_eq(
                source_cut_digest.as_bytes(),
                authorization.source_cut_digest.as_bytes()
            ),
            "episode projection cutover EventStore source cut mismatch"
        );
        let expected_authorization = cutover_authorization_digest(
            authorization.source_row_id,
            &legacy_state_digest,
            &source_cut_digest,
            &authorization.operator_secret,
        );
        anyhow::ensure!(
            constant_time_eq(
                expected_authorization.as_bytes(),
                authorization.authorization_digest.as_bytes()
            ),
            "episode projection cutover authentication failed"
        );
        Ok(EpisodeProjectionCutoverReceipt {
            projection_version: EPISODE_PROJECTION_VERSION,
            source_row_id: authorization.source_row_id,
            legacy_state_digest,
            source_cut_digest,
            authorization_digest: expected_authorization,
        })
    }

    fn commit_source_cursor(
        &mut self,
        event_store: &EventStore,
        control: &EpisodeProjectionControl,
    ) {
        if let Err(error) = self.commit_source_cursor_checked(event_store, control) {
            warn!(cursor = control.last_source_row_id, %error, "Episode Producer: Limbo-Mirror konnte nicht reconciled werden");
        }
    }

    fn commit_source_cursor_checked(
        &mut self,
        event_store: &EventStore,
        control: &EpisodeProjectionControl,
    ) -> anyhow::Result<()> {
        self.last_event_id = control.last_source_row_id;
        let mirror = event_store.get_offset(OFFSET_NAME)?;
        match mirror {
            Some(current) if current > self.last_event_id => {
                event_store.force_reset_offset(OFFSET_NAME, self.last_event_id)
            }
            Some(_) | None => event_store.update_offset(OFFSET_NAME, self.last_event_id),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn quarantine_event(
        &mut self,
        event_store: &EventStore,
        source_row_id: i64,
        event: &DomainEvent,
        affected_subject: Option<EpisodeProjectionSubject>,
        request_digest: String,
        effect_reference_tick: u64,
        reason: EpisodeProjectionQuarantineReason,
        diagnostic: &str,
    ) -> bool {
        let record = EpisodeProjectionQuarantine {
            affected_subject,
            source_event_id: event.event_id.clone(),
            source_row_id,
            event_type: event.event_type.clone(),
            projection_version: EPISODE_PROJECTION_VERSION,
            request_digest,
            effect_reference_tick,
            reason,
            diagnostic_digest: quarantine_diagnostic_digest(diagnostic),
        };
        match self
            .hippocampus
            .store()
            .quarantine_episode_projection(&record, self.last_event_id)
        {
            Ok(control) => {
                self.commit_source_cursor(event_store, &control);
                if let Err(error) = self.refresh_admission_state() {
                    warn!(%error, "Episode Producer: quarantine admission snapshot failed closed");
                }
                true
            }
            Err(error) => {
                warn!(source_row_id, event_id = %event.event_id, %error, "Episode Producer: Quarantaene-Commit fehlgeschlagen");
                false
            }
        }
    }

    /// Retry one durable quarantine after an authenticated operator request.
    pub fn resolve_quarantine(
        &mut self,
        event_store: &EventStore,
        _current_tick: u64,
        _tick_rate_s: f64,
        request: &EpisodeProjectionResolveRequest,
    ) -> anyhow::Result<EpisodeProjectionResolveResponse> {
        anyhow::ensure!(
            request.source_row_id > 0,
            "resolution source row must be positive"
        );
        anyhow::ensure!(
            is_sha256_hex(&request.request_digest) && is_sha256_hex(&request.quarantine_digest),
            "resolution digests must be SHA-256 hex"
        );
        let event = load_exact_source_event(event_store, request.source_row_id)?;
        anyhow::ensure!(
            event.event_id == request.source_event_id,
            "resolution source event identity mismatch"
        );
        let request_digest = source_request_digest(&event);
        anyhow::ensure!(
            constant_time_eq(request_digest.as_bytes(), request.request_digest.as_bytes()),
            "resolution source request digest mismatch"
        );
        anyhow::ensure!(
            is_episode_event_type(&event.event_type),
            "resolution source event is not episode-relevant"
        );
        let payload: DomainEventPayload = serde_json::from_str(&event.payload)
            .map_err(|_| anyhow::anyhow!("resolution source payload remains malformed"))?;
        anyhow::ensure!(
            payload.event_type_str() == event.event_type,
            "resolution source envelope/payload type mismatch remains unresolved"
        );
        let (subject, stable_agent_name) = self
            .episode_agent(&payload)
            .ok_or_else(|| anyhow::anyhow!("resolution source agent remains unregistered"))?;
        let episode_id = stable_episode_id(
            subject,
            &event.event_id,
            EPISODE_PROJECTION_VERSION,
            &request_digest,
        );
        let control = self
            .hippocampus
            .store()
            .load_episode_projection_control()?
            .ok_or_else(|| anyhow::anyhow!("episode projection is not initialized"))?;
        let source_cut = self.authoritative_source_cut(
            event_store,
            control.start_policy.source_row_id(),
            event_store.get_latest_event_id()?,
        )?;
        let effect_reference_tick = source_cut
            .entries
            .iter()
            .find(|entry| entry.source_row_id == request.source_row_id)
            .ok_or_else(|| anyhow::anyhow!("resolution source row is absent from coverage"))?
            .effect_reference_tick;
        let (agent_name, episode) = self
            .event_to_episode(
                &payload,
                episode_id,
                episode_hours_ago(
                    event.tick,
                    effect_reference_tick,
                    source_cut.coverage.tick_duration_millis,
                ),
            )
            .ok_or_else(|| anyhow::anyhow!("resolution source event cannot become an episode"))?;
        anyhow::ensure!(
            agent_name == stable_agent_name,
            "resolution source agent binding changed"
        );

        if let Some(receipt) = self
            .hippocampus
            .store()
            .load_episode_source_receipt(subject, &event.event_id)?
        {
            anyhow::ensure!(
                receipt.source_row_id == request.source_row_id
                    && receipt.request_digest == request.request_digest
                    && receipt.episode_id == episode_id,
                "resolution replay conflicts with the durable source receipt"
            );
            anyhow::ensure!(
                self.hippocampus
                    .store()
                    .load_episode_projection_quarantine(
                        request.source_row_id,
                        &request.source_event_id,
                    )?
                    .is_none(),
                "resolved receipt still has a durable quarantine"
            );
            return Ok(EpisodeProjectionResolveResponse {
                resolved: true,
                duplicate: true,
                source_row_id: request.source_row_id,
                source_event_id: request.source_event_id.clone(),
                episode_id,
            });
        }

        let quarantine = self
            .hippocampus
            .store()
            .load_episode_projection_quarantine(request.source_row_id, &request.source_event_id)?
            .ok_or_else(|| anyhow::anyhow!("episode projection quarantine not found"))?;
        anyhow::ensure!(
            constant_time_eq(
                quarantine_record_digest(&quarantine).as_bytes(),
                request.quarantine_digest.as_bytes()
            ),
            "episode projection quarantine digest CAS conflict"
        );
        let write = EpisodeProjectionWrite {
            subject,
            agent_name,
            source_event_id: event.event_id.clone(),
            source_row_id: request.source_row_id,
            projection_version: EPISODE_PROJECTION_VERSION,
            request_digest,
            expected_global_frontier: self.last_event_id,
            effect_reference_tick,
            episode,
        };
        let outcome = self
            .hippocampus
            .store()
            .resolve_episode_projection(&EpisodeProjectionResolution { quarantine, write })?;
        let (duplicate, control, receipt) = match outcome {
            EpisodeProjectionApplyOutcome::Applied {
                control, receipt, ..
            } => (false, control, receipt),
            EpisodeProjectionApplyOutcome::Duplicate {
                control, receipt, ..
            } => (true, control, receipt),
        };
        self.commit_source_cursor(event_store, &control);
        self.refresh_admission_state()?;
        Ok(EpisodeProjectionResolveResponse {
            resolved: true,
            duplicate,
            source_row_id: request.source_row_id,
            source_event_id: request.source_event_id.clone(),
            episode_id: receipt.episode_id,
        })
    }

    fn authoritative_source_cut(
        &self,
        event_store: &EventStore,
        from_exclusive_source_row_id: i64,
        through_source_row_id: i64,
    ) -> anyhow::Result<EpisodeProjectionSourceCutEvidence> {
        self.authoritative_source_material(
            event_store,
            from_exclusive_source_row_id,
            through_source_row_id,
        )
        .map(|(evidence, _)| evidence)
    }

    fn authoritative_source_material(
        &self,
        event_store: &EventStore,
        from_exclusive_source_row_id: i64,
        through_source_row_id: i64,
    ) -> anyhow::Result<(EpisodeProjectionSourceCutEvidence, Vec<(i64, DomainEvent)>)> {
        anyhow::ensure!(
            from_exclusive_source_row_id >= 0
                && through_source_row_id >= from_exclusive_source_row_id
                && through_source_row_id <= event_store.get_latest_event_id()?,
            "episode projection generation source cut is outside EventStore"
        );
        let mut entries = Vec::new();
        let mut source_rows = Vec::new();
        let mut cursor = from_exclusive_source_row_id;
        let mut effect_reference_tick = 0_u64;
        while cursor < through_source_row_id {
            let batch = event_store.get_events_since_with_id(cursor, BATCH_LIMIT)?;
            if batch.is_empty() {
                break;
            }
            let mut progressed = false;
            for (source_row_id, event) in batch {
                if source_row_id > through_source_row_id {
                    break;
                }
                anyhow::ensure!(
                    source_row_id > cursor,
                    "EventStore source coverage is not strictly ordered"
                );
                let request_digest = source_request_digest(&event);
                let classification = self.classify_source_event(&event);
                effect_reference_tick = effect_reference_tick.max(event.tick);
                entries.push(EpisodeProjectionSourceCoverageEntry {
                    source_row_id,
                    source_event_id: event.event_id.clone(),
                    source_tick: event.tick,
                    effect_reference_tick,
                    request_digest,
                    classification,
                });
                source_rows.push((source_row_id, event));
                cursor = source_row_id;
                progressed = true;
            }
            if !progressed {
                break;
            }
        }
        anyhow::ensure!(
            through_source_row_id == from_exclusive_source_row_id
                || cursor == through_source_row_id,
            "episode projection source cut endpoint is absent or discarded"
        );
        let coverage = episode_projection_source_cut_coverage(
            from_exclusive_source_row_id,
            through_source_row_id,
            self.tick_duration_millis,
            &entries,
        )?;
        Ok((
            EpisodeProjectionSourceCutEvidence { coverage, entries },
            source_rows,
        ))
    }

    fn classify_source_event(&self, event: &DomainEvent) -> EpisodeProjectionSourceClassification {
        if !is_episode_event_type(&event.event_type) {
            return EpisodeProjectionSourceClassification::Irrelevant;
        }
        let envelope_subject = projection_subject_from_event(event)
            .and_then(|subject| self.is_registered_subject(subject).then_some(subject));
        let payload: DomainEventPayload = match serde_json::from_str(&event.payload) {
            Ok(payload) => payload,
            Err(_) => {
                return EpisodeProjectionSourceClassification::Quarantined {
                    affected_subject: envelope_subject,
                    reason: EpisodeProjectionQuarantineReason::MalformedRelevantPayload,
                };
            }
        };
        let payload_subject = episode_subject(&payload)
            .and_then(|subject| self.is_registered_subject(subject).then_some(subject));
        if payload.event_type_str() != event.event_type {
            return EpisodeProjectionSourceClassification::Quarantined {
                affected_subject: payload_subject,
                reason: EpisodeProjectionQuarantineReason::EventTypeMismatch,
            };
        }
        if validate_episode_payload(&payload).is_err() {
            return EpisodeProjectionSourceClassification::Quarantined {
                affected_subject: payload_subject,
                reason: EpisodeProjectionQuarantineReason::MalformedRelevantPayload,
            };
        }
        match self.episode_agent(&payload) {
            Some((subject, _)) => EpisodeProjectionSourceClassification::Episode { subject },
            None => EpisodeProjectionSourceClassification::Quarantined {
                affected_subject: payload_subject,
                reason: EpisodeProjectionQuarantineReason::UnknownAgent,
            },
        }
    }

    fn is_registered_subject(&self, subject: EpisodeProjectionSubject) -> bool {
        match subject {
            EpisodeProjectionSubject::Agent { agent_id } => {
                self.agent_names.contains_key(&agent_id.0)
            }
            EpisodeProjectionSubject::Building => true,
        }
    }

    fn build_authoritative_generation_candidate(
        &self,
        event_store: &EventStore,
        expected_active_generation_id: &str,
    ) -> anyhow::Result<(
        EpisodeProjectionGenerationCandidate,
        EpisodeProjectionSourceCutEvidence,
    )> {
        let status = self
            .hippocampus
            .store()
            .load_episode_projection_generation_status()?;
        anyhow::ensure!(
            status.active_generation_id == expected_active_generation_id,
            "episode projection active generation changed"
        );
        let persisted_control = self
            .hippocampus
            .store()
            .load_episode_projection_control()?
            .ok_or_else(|| anyhow::anyhow!("episode projection is not initialized"))?;
        let source_start = persisted_control.start_policy.source_row_id();
        let source_head = event_store.get_latest_event_id()?;
        let (evidence, source_rows) =
            self.authoritative_source_material(event_store, source_start, source_head)?;

        let mut agents: Vec<EpisodeProjectionAgent> = self
            .agent_names
            .iter()
            .map(|(agent_id, agent_name)| EpisodeProjectionAgent {
                subject: EpisodeProjectionSubject::Agent {
                    agent_id: AgentId(*agent_id),
                },
                agent_name: agent_name.clone(),
            })
            .collect();
        agents.push(EpisodeProjectionAgent {
            subject: EpisodeProjectionSubject::Building,
            agent_name: "_building".to_string(),
        });
        agents.sort_by_key(|agent| match agent.subject {
            EpisodeProjectionSubject::Agent { agent_id } => (0, agent_id.0),
            EpisodeProjectionSubject::Building => (1, 0),
        });
        let mut subjects = Vec::with_capacity(agents.len());
        for agent in agents {
            let archived_episodes = self.hippocampus.store().load_archive(&agent.agent_name)?;
            subjects.push(EpisodeProjectionGenerationSubject {
                frontier: sentinel_hippocampus::EpisodeProjectionFrontier {
                    subject: agent.subject,
                    agent_name: agent.agent_name.clone(),
                    projection_version: EPISODE_PROJECTION_VERSION,
                    start_policy: persisted_control.start_policy.clone(),
                    last_source_row_id: source_start,
                    last_source_event_id: None,
                    last_request_digest: None,
                    applied_count: 0,
                },
                agent,
                receipts: Vec::new(),
                live_episodes: Vec::new(),
                archived_episodes,
                coverage_digest: String::new(),
            });
        }
        let mut control = EpisodeProjectionControl {
            projection_version: EPISODE_PROJECTION_VERSION,
            start_policy: persisted_control.start_policy.clone(),
            last_source_row_id: source_start,
            last_source_event_id: None,
            effect_reference_tick: 0,
        };
        let mut quarantines = Vec::new();

        anyhow::ensure!(
            source_rows.len() == evidence.entries.len(),
            "episode projection source material cardinality mismatch"
        );
        for ((source_row_id, event), entry) in source_rows.iter().zip(&evidence.entries) {
            anyhow::ensure!(
                *source_row_id == entry.source_row_id
                    && event.event_id == entry.source_event_id
                    && source_request_digest(event) == entry.request_digest,
                "episode projection source material identity mismatch"
            );
            control.last_source_row_id = *source_row_id;
            control.last_source_event_id = Some(event.event_id.clone());
            control.effect_reference_tick = entry.effect_reference_tick;
            match &entry.classification {
                EpisodeProjectionSourceClassification::Irrelevant => {}
                EpisodeProjectionSourceClassification::Episode { subject } => {
                    let payload: DomainEventPayload = serde_json::from_str(&event.payload)
                        .map_err(|_| anyhow::anyhow!("episode-classified source is malformed"))?;
                    anyhow::ensure!(
                        payload.event_type_str() == event.event_type,
                        "episode-classified source type changed"
                    );
                    let (derived_subject, expected_agent_name) =
                        self.episode_agent(&payload).ok_or_else(|| {
                            anyhow::anyhow!("episode-classified subject is unavailable")
                        })?;
                    anyhow::ensure!(
                        derived_subject == *subject,
                        "episode-classified subject changed"
                    );
                    let episode_id = stable_episode_id(
                        *subject,
                        &event.event_id,
                        EPISODE_PROJECTION_VERSION,
                        &entry.request_digest,
                    );
                    let (agent_name, episode) = self
                        .event_to_episode(
                            &payload,
                            episode_id,
                            episode_hours_ago(
                                entry.source_tick,
                                entry.effect_reference_tick,
                                evidence.coverage.tick_duration_millis,
                            ),
                        )
                        .ok_or_else(|| {
                            anyhow::anyhow!("episode-classified source has no effect")
                        })?;
                    anyhow::ensure!(
                        agent_name == expected_agent_name,
                        "episode-classified storage locator changed"
                    );
                    let candidate_subject = subjects
                        .iter_mut()
                        .find(|candidate| candidate.agent.subject == *subject)
                        .ok_or_else(|| {
                            anyhow::anyhow!("episode subject is absent from generation")
                        })?;
                    candidate_subject.receipts.push(EpisodeSourceReceipt {
                        subject: *subject,
                        agent_name: agent_name.clone(),
                        source_event_id: event.event_id.clone(),
                        source_row_id: *source_row_id,
                        projection_version: EPISODE_PROJECTION_VERSION,
                        request_digest: entry.request_digest.clone(),
                        episode_id,
                        effect_reference_tick: entry.effect_reference_tick,
                    });
                    let archived_episode = candidate_subject
                        .archived_episodes
                        .iter()
                        .find(|archived| archived.id == episode.id);
                    if let Some(archived) = archived_episode {
                        anyhow::ensure!(
                            serde_json::to_vec(archived)? == serde_json::to_vec(&episode)?,
                            "archived episode identity conflicts with authoritative replay"
                        );
                    } else {
                        candidate_subject.live_episodes.push(episode);
                        if candidate_subject.live_episodes.len()
                            > EPISODE_PROJECTION_MAX_LIVE_EPISODES_PER_SUBJECT
                        {
                            let excess = candidate_subject.live_episodes.len()
                                - EPISODE_PROJECTION_MAX_LIVE_EPISODES_PER_SUBJECT;
                            candidate_subject.live_episodes.drain(..excess);
                        }
                    }
                    candidate_subject.frontier.last_source_row_id = *source_row_id;
                    candidate_subject.frontier.last_source_event_id = Some(event.event_id.clone());
                    candidate_subject.frontier.last_request_digest =
                        Some(entry.request_digest.clone());
                    candidate_subject.frontier.applied_count =
                        candidate_subject.frontier.applied_count.saturating_add(1);
                }
                EpisodeProjectionSourceClassification::Quarantined {
                    affected_subject,
                    reason,
                } => quarantines.push(EpisodeProjectionQuarantine {
                    affected_subject: *affected_subject,
                    source_event_id: event.event_id.clone(),
                    source_row_id: *source_row_id,
                    event_type: event.event_type.clone(),
                    projection_version: EPISODE_PROJECTION_VERSION,
                    request_digest: entry.request_digest.clone(),
                    effect_reference_tick: entry.effect_reference_tick,
                    reason: reason.clone(),
                    diagnostic_digest: generation_quarantine_diagnostic_digest(entry)?,
                }),
            }
        }
        for subject in &mut subjects {
            subject.coverage_digest =
                sentinel_hippocampus::episode_projection_subject_coverage_digest(
                    &subject.agent,
                    &subject.frontier,
                    &subject.receipts,
                    &subject.live_episodes,
                    &subject.archived_episodes,
                )?;
        }
        let archive_snapshot_digest =
            sentinel_hippocampus::episode_projection_archive_snapshot_digest(&subjects)?;
        let descriptor = EpisodeProjectionGenerationDescriptor {
            generation_id: sentinel_hippocampus::episode_projection_generation_id(
                Some(expected_active_generation_id),
                EPISODE_PROJECTION_VERSION,
                &evidence.coverage,
                &archive_snapshot_digest,
            ),
            parent_generation_id: Some(expected_active_generation_id.to_string()),
            projection_version: EPISODE_PROJECTION_VERSION,
            source_cut: evidence.coverage.clone(),
            archive_snapshot_digest,
        };
        Ok((
            EpisodeProjectionGenerationCandidate {
                descriptor,
                control,
                subjects,
                quarantines,
                source_coverage: evidence.entries.clone(),
            },
            evidence,
        ))
    }

    fn stage_generation_candidate(
        &self,
        event_store: &EventStore,
        proposed: &EpisodeProjectionGenerationCandidate,
        expected_active_generation_id: &str,
    ) -> anyhow::Result<String> {
        let (expected, evidence) = self
            .build_authoritative_generation_candidate(event_store, expected_active_generation_id)?;
        anyhow::ensure!(
            serde_json::to_vec(proposed)? == serde_json::to_vec(&expected)?,
            "episode projection generation effects differ from authoritative replay"
        );
        self.hippocampus
            .store()
            .begin_episode_projection_generation(proposed, expected_active_generation_id, &evidence)
    }

    fn source_evidence_for_generation(
        &self,
        event_store: &EventStore,
        status: &EpisodeProjectionGenerationStatus,
        generation_id: &str,
        snapshot_cut: bool,
    ) -> anyhow::Result<EpisodeProjectionSourceCutEvidence> {
        let generation = status
            .generations
            .iter()
            .find(|generation| generation.descriptor.generation_id == generation_id)
            .ok_or_else(|| anyhow::anyhow!("episode projection generation not found"))?;
        let cut = if snapshot_cut {
            &generation.snapshot_source_cut
        } else {
            &generation.descriptor.source_cut
        };
        self.authoritative_source_cut(
            event_store,
            cut.from_exclusive_source_row_id,
            cut.through_source_row_id,
        )
    }

    /// Authenticated operator boundary for the narrow #735 generation
    /// lifecycle. Stage accepts only the active-generation CAS; all source and
    /// effect material is rebuilt twice from EventStore before the first write.
    /// Responses contain only typed IDs, digests, counts, and phase.
    pub fn handle_generation_request(
        &mut self,
        event_store: &EventStore,
        request: &EpisodeProjectionGenerationRequest,
    ) -> anyhow::Result<EpisodeProjectionGenerationResponse> {
        let (operation, generation_id, candidate_digest) = match request {
            EpisodeProjectionGenerationRequest::Stage {
                expected_active_generation_id,
            } => {
                let (candidate, _) = self.build_authoritative_generation_candidate(
                    event_store,
                    expected_active_generation_id,
                )?;
                let digest = self.stage_generation_candidate(
                    event_store,
                    &candidate,
                    expected_active_generation_id,
                )?;
                (
                    "stage".to_string(),
                    Some(candidate.descriptor.generation_id.clone()),
                    Some(digest),
                )
            }
            EpisodeProjectionGenerationRequest::Validate {
                generation_id,
                expected_active_generation_id,
            } => {
                let status = self
                    .hippocampus
                    .store()
                    .load_episode_projection_generation_status()?;
                let evidence = self.source_evidence_for_generation(
                    event_store,
                    &status,
                    generation_id,
                    false,
                )?;
                let digest = self
                    .hippocampus
                    .store()
                    .validate_episode_projection_generation(
                        generation_id,
                        expected_active_generation_id,
                        &evidence,
                    )?;
                (
                    "validate".to_string(),
                    Some(generation_id.clone()),
                    Some(digest),
                )
            }
            EpisodeProjectionGenerationRequest::Discard {
                generation_id,
                expected_active_generation_id,
                expected_candidate_digest,
            } => {
                self.hippocampus
                    .store()
                    .discard_episode_projection_generation(
                        generation_id,
                        expected_active_generation_id,
                        expected_candidate_digest,
                    )?;
                (
                    "discard".to_string(),
                    Some(generation_id.clone()),
                    Some(expected_candidate_digest.clone()),
                )
            }
            EpisodeProjectionGenerationRequest::Activate {
                generation_id,
                expected_active_generation_id,
                expected_candidate_digest,
            }
            | EpisodeProjectionGenerationRequest::Rollback {
                generation_id,
                expected_active_generation_id,
                expected_candidate_digest,
            } => {
                let status = self
                    .hippocampus
                    .store()
                    .load_episode_projection_generation_status()?;
                anyhow::ensure!(
                    status.active_generation_id == *expected_active_generation_id,
                    "episode projection active generation changed"
                );
                let target = status
                    .generations
                    .iter()
                    .find(|generation| generation.descriptor.generation_id == *generation_id)
                    .ok_or_else(|| anyhow::anyhow!("episode projection generation not found"))?;
                let rollback =
                    matches!(request, EpisodeProjectionGenerationRequest::Rollback { .. });
                let target_evidence = self.source_evidence_for_generation(
                    event_store,
                    &status,
                    generation_id,
                    rollback,
                )?;
                let control = self
                    .hippocampus
                    .store()
                    .load_episode_projection_control()?
                    .ok_or_else(|| anyhow::anyhow!("episode projection is not initialized"))?;
                let active_generation = status
                    .generations
                    .iter()
                    .find(|generation| {
                        generation.descriptor.generation_id == status.active_generation_id
                    })
                    .ok_or_else(|| {
                        anyhow::anyhow!("active episode projection generation missing")
                    })?;
                let active_evidence = self.authoritative_source_cut(
                    event_store,
                    active_generation
                        .descriptor
                        .source_cut
                        .from_exclusive_source_row_id,
                    control.last_source_row_id,
                )?;
                if rollback {
                    anyhow::ensure!(
                        target.phase == EpisodeProjectionGenerationPhase::Retained,
                        "rollback target is not retained"
                    );
                    self.hippocampus
                        .store()
                        .rollback_episode_projection_generation(
                            generation_id,
                            expected_active_generation_id,
                            expected_candidate_digest,
                            &target_evidence,
                            &active_evidence,
                        )?;
                } else {
                    anyhow::ensure!(
                        target.phase == EpisodeProjectionGenerationPhase::Validated,
                        "activation target is not validated"
                    );
                    self.hippocampus
                        .store()
                        .activate_episode_projection_generation(
                            generation_id,
                            expected_active_generation_id,
                            expected_candidate_digest,
                            &target_evidence,
                            &active_evidence,
                        )?;
                }
                let control = self
                    .hippocampus
                    .store()
                    .load_episode_projection_control()?
                    .ok_or_else(|| anyhow::anyhow!("episode projection is not initialized"))?;
                self.commit_source_cursor_checked(event_store, &control)?;
                (
                    if rollback { "rollback" } else { "activate" }.to_string(),
                    Some(generation_id.clone()),
                    Some(expected_candidate_digest.clone()),
                )
            }
            EpisodeProjectionGenerationRequest::Status => ("status".to_string(), None, None),
        };
        self.refresh_admission_state()?;
        Ok(EpisodeProjectionGenerationResponse {
            operation,
            generation_id,
            candidate_digest,
            status: self
                .hippocampus
                .store()
                .load_episode_projection_generation_status()?,
        })
    }

    pub fn admission_snapshot(&self) -> EpisodeProjectionAdmissionSnapshot {
        self.admission_state
            .read()
            .map(|state| state.clone())
            .unwrap_or_default()
    }

    fn refresh_admission_state(&self) -> anyhow::Result<()> {
        let result = self.build_admission_snapshot();
        let snapshot = match result {
            Ok(snapshot) => snapshot,
            Err(error) => {
                if let Ok(mut state) = self.admission_state.write() {
                    *state = EpisodeProjectionAdmissionSnapshot::default();
                }
                return Err(error);
            }
        };
        let mut state = self
            .admission_state
            .write()
            .map_err(|_| anyhow::anyhow!("episode projection admission lock poisoned"))?;
        *state = snapshot;
        Ok(())
    }

    fn build_admission_snapshot(&self) -> anyhow::Result<EpisodeProjectionAdmissionSnapshot> {
        let control = self
            .hippocampus
            .store()
            .load_episode_projection_control()?
            .ok_or_else(|| anyhow::anyhow!("episode projection is uninitialized"))?;
        let quarantines = self
            .hippocampus
            .store()
            .list_episode_projection_quarantine()?;
        let global_blockers = quarantines
            .iter()
            .filter(|record| {
                !matches!(
                    record.affected_subject,
                    Some(EpisodeProjectionSubject::Agent { .. })
                )
            })
            .map(blocker_diagnostic)
            .collect();
        let mut agents = Vec::new();
        let mut identities: Vec<(u16, &String)> = self
            .agent_names
            .iter()
            .map(|(id, name)| (*id, name))
            .collect();
        identities.sort_by_key(|(id, _)| *id);
        for (agent_id, _) in identities {
            let subject = EpisodeProjectionSubject::Agent {
                agent_id: AgentId(agent_id),
            };
            let readiness = self
                .hippocampus
                .store()
                .load_episode_projection_readiness(subject)?;
            let blockers = quarantines
                .iter()
                .filter(|record| record.affected_subject == Some(subject))
                .map(blocker_diagnostic)
                .collect::<Vec<_>>();
            let frontier_source_row_id = readiness
                .frontier
                .as_ref()
                .map(|frontier| frontier.last_source_row_id);
            agents.push(EpisodeProjectionAgentDiagnostic {
                agent_id,
                ready: readiness.is_ready(),
                frontier_source_row_id,
                lag_rows: frontier_source_row_id
                    .map(|frontier| control.last_source_row_id.saturating_sub(frontier)),
                blockers,
            });
        }
        Ok(EpisodeProjectionAdmissionSnapshot {
            initialized: true,
            integrity_error: false,
            global_frontier_source_row_id: Some(control.last_source_row_id),
            global_blockers,
            agents,
        })
    }

    fn episode_agent(
        &self,
        payload: &DomainEventPayload,
    ) -> Option<(EpisodeProjectionSubject, String)> {
        let subject = episode_subject(payload)?;
        match subject {
            EpisodeProjectionSubject::Agent { agent_id } => self
                .agent_names
                .get(&agent_id.0)
                .cloned()
                .map(|name| (subject, name)),
            EpisodeProjectionSubject::Building => Some((subject, "_building".to_string())),
        }
    }

    /// Konvertiert einen DomainEventPayload in eine Episode (wenn relevant).
    fn event_to_episode(
        &self,
        payload: &DomainEventPayload,
        episode_id: u64,
        hours_ago: f64,
    ) -> Option<(String, Episode)> {
        match payload {
            DomainEventPayload::AgentActionReceived {
                agent_id,
                action_type,
                content,
                target_room,
                ..
            } => {
                let name = self.agent_names.get(&agent_id.0)?.clone();
                let (relevance, emotion, tags) = classify_action(action_type, content.as_deref());
                let summary = format_action_summary(
                    &name,
                    action_type,
                    content.as_deref(),
                    target_room.as_deref(),
                );

                Some((
                    name.clone(),
                    Episode {
                        id: episode_id,
                        agent_name: name,
                        summary,
                        relevance,
                        emotion,
                        repetitions: 1,
                        hours_ago,
                        participants: vec![],
                        tags,
                    },
                ))
            }

            DomainEventPayload::BioActionPerformed { agent_id, action } => {
                let name = self.agent_names.get(&agent_id.0)?.clone();
                Some((
                    name.clone(),
                    Episode {
                        id: episode_id,
                        agent_name: name,
                        summary: format!("Bio: {action}"),
                        relevance: 0.1,
                        emotion: 0.05,
                        repetitions: 1,
                        hours_ago,
                        participants: vec![],
                        tags: vec!["routine".to_string(), "bio".to_string()],
                    },
                ))
            }

            DomainEventPayload::ChaosTriggered {
                event_type,
                description,
                ..
            } => {
                // Chaos-Events werden als gebaeude-weite Episoden gespeichert.
                // Nightrun kann sie fuer alle betroffenen Agents aggregieren.
                let summary = format!("Chaos: {event_type:?} - {description}");
                Some((
                    "_building".to_string(),
                    Episode {
                        id: episode_id,
                        agent_name: "_building".to_string(),
                        summary,
                        relevance: 0.7,
                        emotion: 0.6,
                        repetitions: 1,
                        hours_ago,
                        participants: vec![],
                        tags: vec!["chaos".to_string(), format!("{event_type:?}")],
                    },
                ))
            }

            DomainEventPayload::ProjectCloseoutPublished {
                project_id,
                release_id,
                acceptance_id,
                candidate_digest,
                lessons_digest,
                ..
            } => {
                let candidate_prefix = candidate_digest.get(..16)?;
                let lessons_prefix = lessons_digest.get(..16)?;
                Some((
                    "_building".to_string(),
                    Episode {
                    id: episode_id,
                    agent_name: "_building".to_string(),
                    summary: format!(
                        "Project {project_id} closed with accepted release {release_id} ({acceptance_id})"
                    ),
                    relevance: 0.95,
                    emotion: 0.35,
                    repetitions: 1,
                    hours_ago,
                    participants: vec![],
                    tags: vec![
                        "project_closeout".to_string(),
                        "customer_accepted".to_string(),
                        format!("candidate:{candidate_prefix}"),
                        format!("lessons:{lessons_prefix}"),
                    ],
                },
                ))
            }

            // Andere Event-Typen sind nicht episoden-relevant
            _ => None,
        }
    }
}

fn projection_identity_map(agents: &[(u16, String)]) -> anyhow::Result<HashMap<u16, String>> {
    let mut identities = HashMap::new();
    let mut names = HashMap::new();
    for (agent_id, agent_name) in agents {
        if let Some(existing_name) = identities.get(agent_id) {
            anyhow::ensure!(
                existing_name == agent_name,
                "current roster contains conflicting names for agent {agent_id}"
            );
        }
        if let Some(existing_id) = names.get(agent_name) {
            anyhow::ensure!(
                existing_id == agent_id,
                "current roster name {agent_name} is bound to multiple agents"
            );
        }
        anyhow::ensure!(
            agent_name != "_building",
            "current roster cannot use the Building projection storage locator"
        );
        identities.insert(*agent_id, agent_name.clone());
        names.insert(agent_name.clone(), *agent_id);
    }
    Ok(identities)
}

fn is_episode_event_type(event_type: &str) -> bool {
    matches!(
        event_type,
        "agent_action_received"
            | "bio_action_performed"
            | "chaos_triggered"
            | "project_closeout_published"
    )
}

fn source_request_digest(event: &DomainEvent) -> String {
    let mut digest = Sha256::new();
    digest.update(b"sentinel-episode-projection-request-v1\0");
    digest_field(&mut digest, event.event_id.as_bytes());
    digest_field(&mut digest, event.event_type.as_bytes());
    digest_field(&mut digest, event.aggregate_id.as_bytes());
    digest_field(&mut digest, event.payload.as_bytes());
    digest_field(&mut digest, event.correlation_id.as_bytes());
    match &event.causation_id {
        Some(causation_id) => {
            digest.update([1]);
            digest_field(&mut digest, causation_id.as_bytes());
        }
        None => digest.update([0]),
    }
    digest_field(&mut digest, event.operation_id.as_bytes());
    digest.update(event.tick.to_be_bytes());
    digest.update(event.timestamp_ms.to_be_bytes());
    digest.update(event.schema_version.to_be_bytes());
    digest_field(&mut digest, event.compensation_type.as_bytes());
    format!("{:x}", digest.finalize())
}

fn episode_hours_ago(event_tick: u64, reference_tick: u64, tick_duration_millis: u64) -> f64 {
    let elapsed_millis = reference_tick
        .saturating_sub(event_tick)
        .saturating_mul(tick_duration_millis);
    elapsed_millis as f64 / 3_600_000.0
}

fn generation_quarantine_diagnostic_digest(
    entry: &EpisodeProjectionSourceCoverageEntry,
) -> anyhow::Result<String> {
    let encoded = serde_json::to_vec(entry)?;
    let mut digest = Sha256::new();
    digest.update(b"sentinel-episode-generation-quarantine-v1\0");
    digest_field(&mut digest, &encoded);
    Ok(format!("{:x}", digest.finalize()))
}

pub fn event_store_source_cut_digest(
    event_store: &EventStore,
    source_row_id: i64,
) -> anyhow::Result<String> {
    anyhow::ensure!(source_row_id >= 0, "source cut row must be non-negative");
    anyhow::ensure!(
        source_row_id <= event_store.get_latest_event_id()?,
        "source cut row is beyond the EventStore head"
    );
    let mut digest = Sha256::new();
    digest.update(b"sentinel-episode-eventstore-cut-v1\0");
    digest.update(source_row_id.to_be_bytes());
    let mut cursor = 0_i64;
    let mut count = 0_u64;
    while cursor < source_row_id {
        let batch = event_store.get_events_since_with_id(cursor, BATCH_LIMIT)?;
        if batch.is_empty() {
            break;
        }
        let mut progressed = false;
        for (row_id, event) in batch {
            if row_id > source_row_id {
                break;
            }
            digest.update(row_id.to_be_bytes());
            digest_field(&mut digest, event.event_id.as_bytes());
            digest_field(&mut digest, source_request_digest(&event).as_bytes());
            cursor = row_id;
            count = count.saturating_add(1);
            progressed = true;
        }
        if !progressed {
            break;
        }
    }
    anyhow::ensure!(
        source_row_id == 0 || cursor == source_row_id,
        "source cut row is absent or belongs to a discarded EventStore range"
    );
    digest.update(count.to_be_bytes());
    Ok(format!("{:x}", digest.finalize()))
}

pub fn cutover_authorization_digest(
    source_row_id: i64,
    legacy_state_digest: &str,
    source_cut_digest: &str,
    operator_secret: &str,
) -> String {
    let mut digest = Sha256::new();
    digest.update(b"sentinel-episode-cutover-authorization-v1\0");
    digest.update(source_row_id.to_be_bytes());
    digest_field(&mut digest, legacy_state_digest.as_bytes());
    digest_field(&mut digest, source_cut_digest.as_bytes());
    digest_field(&mut digest, operator_secret.as_bytes());
    format!("{:x}", digest.finalize())
}

fn load_exact_source_event(
    event_store: &EventStore,
    source_row_id: i64,
) -> anyhow::Result<DomainEvent> {
    anyhow::ensure!(source_row_id > 0, "source row must be positive");
    let events = event_store.get_events_since_with_id(source_row_id - 1, 1)?;
    match events.as_slice() {
        [(row_id, event)] if *row_id == source_row_id => Ok(event.clone()),
        _ => anyhow::bail!("immutable source row is missing or belongs to a discarded range"),
    }
}

fn quarantine_record_digest(record: &EpisodeProjectionQuarantine) -> String {
    let mut digest = Sha256::new();
    digest.update(b"sentinel-episode-quarantine-record-v1\0");
    let encoded = serde_json::to_vec(record).expect("typed quarantine serializes");
    digest_field(&mut digest, &encoded);
    format!("{:x}", digest.finalize())
}

fn blocker_diagnostic(record: &EpisodeProjectionQuarantine) -> EpisodeProjectionBlockerDiagnostic {
    EpisodeProjectionBlockerDiagnostic {
        source_row_id: record.source_row_id,
        source_event_id: record.source_event_id.clone(),
        reason: record.reason.clone(),
        quarantine_digest: quarantine_record_digest(record),
    }
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn validate_episode_payload(payload: &DomainEventPayload) -> Result<(), &'static str> {
    let DomainEventPayload::ProjectCloseoutPublished {
        tenant_id,
        project_id,
        project_generation,
        project_digest,
        candidate_id,
        candidate_generation,
        candidate_digest,
        release_id,
        release_generation,
        release_digest,
        acceptance_id,
        acceptance_generation,
        acceptance_digest,
        decisions_digest,
        artifact_inventory_digest,
        failures_digest,
        lessons_digest,
    } = payload
    else {
        return Ok(());
    };
    let identities = [
        tenant_id.as_str(),
        project_id.as_str(),
        candidate_id.as_str(),
        release_id.as_str(),
        acceptance_id.as_str(),
    ];
    let digests = [
        project_digest.as_str(),
        candidate_digest.as_str(),
        release_digest.as_str(),
        acceptance_digest.as_str(),
        decisions_digest.as_str(),
        artifact_inventory_digest.as_str(),
        failures_digest.as_str(),
        lessons_digest.as_str(),
    ];
    if identities
        .iter()
        .any(|value| value.is_empty() || value.len() > 128 || value.chars().any(char::is_control))
        || [
            *project_generation,
            *candidate_generation,
            *release_generation,
            *acceptance_generation,
        ]
        .contains(&0)
        || digests.iter().any(|value| !is_sha256_hex(value))
    {
        return Err("project closeout identity, generation, or digest is invalid");
    }
    Ok(())
}

fn stable_episode_id(
    subject: EpisodeProjectionSubject,
    source_event_id: &str,
    projection_version: u32,
    request_digest: &str,
) -> u64 {
    let mut digest = Sha256::new();
    digest.update(b"sentinel-episode-id-v1\0");
    match subject {
        EpisodeProjectionSubject::Agent { agent_id } => {
            digest.update([0]);
            digest.update(agent_id.0.to_be_bytes());
        }
        EpisodeProjectionSubject::Building => digest.update([1]),
    }
    digest_field(&mut digest, source_event_id.as_bytes());
    digest.update(projection_version.to_be_bytes());
    digest_field(&mut digest, request_digest.as_bytes());
    let bytes: [u8; 8] = digest.finalize()[..8]
        .try_into()
        .expect("SHA-256 prefix has eight bytes");
    u64::from_be_bytes(bytes).max(1)
}

fn digest_field(digest: &mut Sha256, value: &[u8]) {
    digest.update((value.len() as u64).to_be_bytes());
    digest.update(value);
}

fn episode_subject(payload: &DomainEventPayload) -> Option<EpisodeProjectionSubject> {
    match payload {
        DomainEventPayload::AgentActionReceived { agent_id, .. }
        | DomainEventPayload::BioActionPerformed { agent_id, .. } => AgentId::new(agent_id.0)
            .ok()
            .map(|agent_id| EpisodeProjectionSubject::Agent { agent_id }),
        DomainEventPayload::ChaosTriggered { .. }
        | DomainEventPayload::ProjectCloseoutPublished { .. } => {
            Some(EpisodeProjectionSubject::Building)
        }
        _ => None,
    }
}

fn projection_subject_from_event(event: &DomainEvent) -> Option<EpisodeProjectionSubject> {
    if matches!(
        event.event_type.as_str(),
        "chaos_triggered" | "project_closeout_published"
    ) {
        return Some(EpisodeProjectionSubject::Building);
    }
    let raw_id = event.aggregate_id.strip_prefix("AGENT-")?;
    let agent_id = raw_id.parse::<u16>().ok()?;
    AgentId::new(agent_id)
        .ok()
        .map(|agent_id| EpisodeProjectionSubject::Agent { agent_id })
}

fn quarantine_diagnostic_digest(value: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"sentinel-episode-quarantine-diagnostic-v1\0");
    digest_field(&mut digest, value.as_bytes());
    format!("{:x}", digest.finalize())
}

/// Klassifiziert eine Agent-Aktion nach Relevanz und Emotion.
fn classify_action(action_type: &str, content: Option<&str>) -> (f64, f64, Vec<String>) {
    let content_lower = content.unwrap_or("").to_lowercase();

    let (relevance, emotion) = match action_type {
        "talk" | "speak" | "say" => {
            if content_lower.contains("konflikt")
                || content_lower.contains("streit")
                || content_lower.contains("problem")
                || content_lower.contains("fehler")
            {
                (0.8, 0.7)
            } else if content_lower.contains("meeting")
                || content_lower.contains("praesentation")
                || content_lower.contains("deadline")
            {
                (0.7, 0.5)
            } else {
                (0.4, 0.3)
            }
        }
        "work" | "code" | "design" | "review" => (0.5, 0.3),
        "move" | "walk" | "goto" => (0.1, 0.05),
        "eat" | "drink" | "coffee" => (0.15, 0.1),
        _ => (0.3, 0.2),
    };

    let mut tags = vec![action_type.to_string()];
    if content_lower.contains("konflikt") || content_lower.contains("streit") {
        tags.push("conflict".to_string());
    }
    if content_lower.contains("meeting") || content_lower.contains("besprechung") {
        tags.push("meeting".to_string());
    }
    if content_lower.contains("lob") || content_lower.contains("gut gemacht") {
        tags.push("praise".to_string());
    }

    (relevance, emotion, tags)
}

/// Erstellt eine lesbare Zusammenfassung einer Agent-Aktion.
fn format_action_summary(
    agent_name: &str,
    action_type: &str,
    content: Option<&str>,
    target_room: Option<&str>,
) -> String {
    let content_part = content
        .map(|c| {
            if c.len() > 80 {
                // UTF-8 safe truncation: find char boundary at or before byte 77
                let truncate_at = c
                    .char_indices()
                    .take_while(|(i, _)| *i <= 77)
                    .last()
                    .map(|(i, _)| i)
                    .unwrap_or(0);
                format!("{}...", &c[..truncate_at])
            } else {
                c.to_string()
            }
        })
        .unwrap_or_default();

    let room_part = target_room.map(|r| format!(" in {r}")).unwrap_or_default();

    if content_part.is_empty() {
        format!("{agent_name}: {action_type}{room_part}")
    } else {
        format!("{agent_name}: {action_type}{room_part} - {content_part}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_hippocampus() -> (HippocampusService, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test-hippocampus.redb");
        let service = HippocampusService::open(path.to_str().unwrap()).unwrap();
        (service, dir)
    }

    fn temp_event_store(dir: &tempfile::TempDir) -> EventStore {
        let path = dir.path().join("test-events.db");
        EventStore::open(path.to_str().unwrap()).unwrap()
    }

    fn append_payload(event_store: &EventStore, payload: &DomainEventPayload, tick: u64) -> i64 {
        let event = DomainEvent::new(
            payload.event_type_str(),
            "AGENT-01",
            &payload.to_json(),
            "episode-producer-test",
            tick,
        );
        event_store.append_event(&event).unwrap()
    }

    #[test]
    fn authoritative_generation_source_cut_classifies_every_eventstore_row() {
        let (hippocampus, dir) = temp_hippocampus();
        let event_store = temp_event_store(&dir);
        let mut producer =
            EpisodeProducer::new(hippocampus, &[(1, "Thomas".to_string())], &event_store).unwrap();
        let irrelevant = DomainEvent::new(
            "task_created",
            "TASK-1",
            r#"{"task_id":"TASK-1"}"#,
            "coverage-test",
            1,
        );
        let first = event_store.append_event(&irrelevant).unwrap();
        let second = append_payload(
            &event_store,
            &DomainEventPayload::BioActionPerformed {
                agent_id: AgentId(1),
                action: "drink".to_string(),
            },
            2,
        );
        let malformed = DomainEvent::new(
            "agent_action_received",
            "AGENT-01",
            "{not-json",
            "coverage-test",
            3,
        );
        let third = event_store.append_event(&malformed).unwrap();
        assert_eq!((first, second, third), (1, 2, 3));

        let evidence = producer
            .authoritative_source_cut(&event_store, 0, third)
            .unwrap();
        assert_eq!(evidence.coverage.event_count, 3);
        assert_eq!(evidence.coverage.irrelevant_count, 1);
        assert_eq!(evidence.coverage.episode_count, 1);
        assert_eq!(evidence.coverage.quarantine_count, 1);
        assert_eq!(evidence.coverage.reference_tick, 3);
        assert_eq!(
            evidence.coverage.tick_duration_millis,
            EPISODE_PROJECTION_TICK_DURATION_MILLIS
        );
        assert!(matches!(
            evidence.entries[0].classification,
            EpisodeProjectionSourceClassification::Irrelevant
        ));
        assert!(matches!(
            evidence.entries[1].classification,
            EpisodeProjectionSourceClassification::Episode {
                subject: EpisodeProjectionSubject::Agent {
                    agent_id: AgentId(1)
                }
            }
        ));
        assert!(matches!(
            evidence.entries[2].classification,
            EpisodeProjectionSourceClassification::Quarantined {
                affected_subject: Some(EpisodeProjectionSubject::Agent {
                    agent_id: AgentId(1)
                }),
                reason: EpisodeProjectionQuarantineReason::MalformedRelevantPayload,
            }
        ));
        assert!(producer
            .authoritative_source_cut(&event_store, 0, third + 1)
            .is_err());

        let status = producer
            .handle_generation_request(&event_store, &EpisodeProjectionGenerationRequest::Status)
            .unwrap();
        assert_eq!(status.operation, "status");
        assert!(status.generation_id.is_none());
    }

    #[test]
    fn restart_hydrates_removed_agent_as_stable_projection_identity() {
        let (hippocampus, dir) = temp_hippocampus();
        let event_store = temp_event_store(&dir);
        let agents = vec![(1, "Thomas".to_string()), (2, "Lisa".to_string())];
        let producer = EpisodeProducer::new(hippocampus, &agents, &event_store).unwrap();
        append_payload(
            &event_store,
            &DomainEventPayload::BioActionPerformed {
                agent_id: AgentId(2),
                action: "historical-drink".to_string(),
            },
            10,
        );
        let active_generation_id = producer
            .hippocampus()
            .store()
            .load_episode_projection_generation_status()
            .unwrap()
            .active_generation_id;
        let (before, before_evidence) = producer
            .build_authoritative_generation_candidate(&event_store, &active_generation_id)
            .unwrap();
        assert_eq!(
            before
                .subjects
                .iter()
                .map(|subject| subject.agent.subject)
                .collect::<Vec<_>>(),
            vec![
                EpisodeProjectionSubject::Agent {
                    agent_id: AgentId(1),
                },
                EpisodeProjectionSubject::Agent {
                    agent_id: AgentId(2),
                },
                EpisodeProjectionSubject::Building,
            ]
        );
        assert!(matches!(
            before_evidence.entries[0].classification,
            EpisodeProjectionSourceClassification::Episode {
                subject: EpisodeProjectionSubject::Agent {
                    agent_id: AgentId(2)
                }
            }
        ));

        drop(producer);
        let reopened =
            HippocampusService::open(dir.path().join("test-hippocampus.redb").to_str().unwrap())
                .unwrap();
        let restarted =
            EpisodeProducer::new(reopened, &[(1, "Thomas".to_string())], &event_store).unwrap();
        assert_eq!(
            restarted.agent_names.get(&1).map(String::as_str),
            Some("Thomas")
        );
        assert_eq!(
            restarted.agent_names.get(&2).map(String::as_str),
            Some("Lisa")
        );
        let (after, after_evidence) = restarted
            .build_authoritative_generation_candidate(&event_store, &active_generation_id)
            .unwrap();
        assert_eq!(
            serde_json::to_vec(&before).unwrap(),
            serde_json::to_vec(&after).unwrap()
        );
        assert_eq!(before_evidence, after_evidence);
    }

    #[test]
    fn restart_rejects_current_roster_conflicts_with_durable_projection_identities() {
        let (hippocampus, dir) = temp_hippocampus();
        let event_store = temp_event_store(&dir);
        let producer = EpisodeProducer::new(
            hippocampus,
            &[(1, "Thomas".to_string()), (2, "Lisa".to_string())],
            &event_store,
        )
        .unwrap();
        drop(producer);

        let reopened =
            HippocampusService::open(dir.path().join("test-hippocampus.redb").to_str().unwrap())
                .unwrap();
        let rename_error = EpisodeProducer::new(
            reopened,
            &[(1, "Thomas".to_string()), (2, "Robert".to_string())],
            &event_store,
        )
        .err()
        .unwrap();
        assert!(rename_error.to_string().contains("immutable"));

        let reopened =
            HippocampusService::open(dir.path().join("test-hippocampus.redb").to_str().unwrap())
                .unwrap();
        let alias_error = EpisodeProducer::new(
            reopened,
            &[(1, "Thomas".to_string()), (3, "Lisa".to_string())],
            &event_store,
        )
        .err()
        .unwrap();
        assert!(alias_error.to_string().contains("already bound"));
    }

    #[test]
    fn generation_operator_rebuilds_effects_and_rejects_candidate_local_tampering() {
        let (hippocampus, dir) = temp_hippocampus();
        let event_store = temp_event_store(&dir);
        let producer =
            EpisodeProducer::new(hippocampus, &[(1, "Thomas".to_string())], &event_store).unwrap();
        let older = DomainEvent::new(
            "bio_action_performed",
            "AGENT-01",
            &DomainEventPayload::BioActionPerformed {
                agent_id: AgentId(1),
                action: "drink".to_string(),
            }
            .to_json(),
            "generation-operator-test",
            3600,
        );
        let newer = DomainEvent::new(
            "bio_action_performed",
            "AGENT-01",
            &DomainEventPayload::BioActionPerformed {
                agent_id: AgentId(1),
                action: "eat".to_string(),
            }
            .to_json(),
            "generation-operator-test",
            0,
        );
        assert_eq!(event_store.append_event(&older).unwrap(), 1);
        assert_eq!(event_store.append_event(&newer).unwrap(), 2);
        let expected_active_generation_id = producer
            .hippocampus()
            .store()
            .load_episode_projection_generation_status()
            .unwrap()
            .active_generation_id;
        assert!(
            serde_json::from_value::<EpisodeProjectionGenerationRequest>(serde_json::json!({
                "action": "stage",
                "expected_active_generation_id": expected_active_generation_id.clone(),
                "candidate": {"untrusted": true}
            }),)
            .is_err()
        );
        assert!(
            serde_json::from_value::<EpisodeProjectionGenerationRequest>(serde_json::json!({
                "action": "discard",
                "generation_id": "11".repeat(32),
                "expected_active_generation_id": "22".repeat(32),
                "expected_candidate_digest": "33".repeat(32),
                "candidate": {"untrusted": true}
            }))
            .is_err()
        );
        let (candidate, evidence) = producer
            .build_authoritative_generation_candidate(&event_store, &expected_active_generation_id)
            .unwrap();
        assert_eq!(candidate.subjects.len(), 2);
        assert_eq!(candidate.subjects[0].agent.agent_name, "Thomas");
        assert_eq!(candidate.subjects[1].agent.agent_name, "_building");
        assert_eq!(candidate.subjects[0].live_episodes[0].hours_ago, 0.0);
        assert_eq!(candidate.subjects[0].live_episodes[1].hours_ago, 1.0);
        assert_eq!(candidate.descriptor.source_cut.reference_tick, 3600);
        assert_eq!(
            candidate.descriptor.source_cut.tick_duration_millis,
            EPISODE_PROJECTION_TICK_DURATION_MILLIS
        );
        let changed_clock_cut = episode_projection_source_cut_coverage(
            0,
            2,
            EPISODE_PROJECTION_TICK_DURATION_MILLIS / 2,
            &evidence.entries,
        )
        .unwrap();
        assert_ne!(
            candidate.descriptor.source_cut.coverage_digest,
            changed_clock_cut.coverage_digest
        );

        drop(producer);
        let reopened =
            HippocampusService::open(dir.path().join("test-hippocampus.redb").to_str().unwrap())
                .unwrap();
        let mut producer =
            EpisodeProducer::new(reopened, &[(1, "Thomas".to_string())], &event_store).unwrap();
        let (rebuilt, _) = producer
            .build_authoritative_generation_candidate(&event_store, &expected_active_generation_id)
            .unwrap();
        assert_eq!(
            serde_json::to_vec(&candidate).unwrap(),
            serde_json::to_vec(&rebuilt).unwrap()
        );
        assert_eq!(
            candidate.descriptor.generation_id,
            rebuilt.descriptor.generation_id
        );
        assert_eq!(
            candidate.subjects[0].coverage_digest,
            rebuilt.subjects[0].coverage_digest
        );
        assert_eq!(producer.tick(&event_store, 900, 7.5), 2);
        assert_eq!(
            serde_json::to_vec(
                &producer
                    .hippocampus()
                    .store()
                    .load_episodes("Thomas")
                    .unwrap(),
            )
            .unwrap(),
            serde_json::to_vec(&rebuilt.subjects[0].live_episodes).unwrap(),
        );

        let mut mutated = rebuilt.clone();
        mutated.subjects[0].live_episodes[0].summary = "caller-forged summary".to_string();
        mutated.subjects[0].coverage_digest =
            sentinel_hippocampus::episode_projection_subject_coverage_digest(
                &mutated.subjects[0].agent,
                &mutated.subjects[0].frontier,
                &mutated.subjects[0].receipts,
                &mutated.subjects[0].live_episodes,
                &mutated.subjects[0].archived_episodes,
            )
            .unwrap();
        assert!(producer
            .stage_generation_candidate(&event_store, &mutated, &expected_active_generation_id,)
            .is_err());

        let mut extra_live = rebuilt.clone();
        let mut fabricated = extra_live.subjects[0].live_episodes[0].clone();
        fabricated.id = fabricated.id.wrapping_add(1);
        fabricated.summary = "fabricated live episode".to_string();
        extra_live.subjects[0]
            .live_episodes
            .push(fabricated.clone());
        extra_live.subjects[0].coverage_digest =
            sentinel_hippocampus::episode_projection_subject_coverage_digest(
                &extra_live.subjects[0].agent,
                &extra_live.subjects[0].frontier,
                &extra_live.subjects[0].receipts,
                &extra_live.subjects[0].live_episodes,
                &extra_live.subjects[0].archived_episodes,
            )
            .unwrap();
        assert!(producer
            .stage_generation_candidate(&event_store, &extra_live, &expected_active_generation_id,)
            .is_err());

        let mut extra_archive = rebuilt.clone();
        fabricated.id = fabricated.id.wrapping_add(1);
        fabricated.summary = "fabricated archived episode".to_string();
        extra_archive.subjects[0].archived_episodes.push(fabricated);
        extra_archive.subjects[0].coverage_digest =
            sentinel_hippocampus::episode_projection_subject_coverage_digest(
                &extra_archive.subjects[0].agent,
                &extra_archive.subjects[0].frontier,
                &extra_archive.subjects[0].receipts,
                &extra_archive.subjects[0].live_episodes,
                &extra_archive.subjects[0].archived_episodes,
            )
            .unwrap();
        assert!(producer
            .stage_generation_candidate(
                &event_store,
                &extra_archive,
                &expected_active_generation_id,
            )
            .is_err());
        let unchanged = producer
            .hippocampus()
            .store()
            .load_episode_projection_generation_status()
            .unwrap();
        assert_eq!(unchanged.generations.len(), 1);
        assert!(producer
            .episode_projection_readiness(AgentId(1))
            .unwrap()
            .is_ready());

        let staged = producer
            .handle_generation_request(
                &event_store,
                &EpisodeProjectionGenerationRequest::Stage {
                    expected_active_generation_id: expected_active_generation_id.clone(),
                },
            )
            .unwrap();
        assert_eq!(staged.operation, "stage");
        assert_eq!(
            staged.generation_id.as_deref(),
            Some(rebuilt.descriptor.generation_id.as_str())
        );
        assert!(staged.candidate_digest.is_some());
        let validated = producer
            .handle_generation_request(
                &event_store,
                &EpisodeProjectionGenerationRequest::Validate {
                    generation_id: candidate.descriptor.generation_id.clone(),
                    expected_active_generation_id: expected_active_generation_id.clone(),
                },
            )
            .unwrap();
        assert_eq!(validated.operation, "validate");
        assert!(validated.status.generations.iter().any(|generation| {
            generation.descriptor.generation_id == rebuilt.descriptor.generation_id
                && generation.phase == EpisodeProjectionGenerationPhase::Validated
        }));
        let discarded = producer
            .handle_generation_request(
                &event_store,
                &EpisodeProjectionGenerationRequest::Discard {
                    generation_id: candidate.descriptor.generation_id.clone(),
                    expected_active_generation_id,
                    expected_candidate_digest: staged.candidate_digest.unwrap(),
                },
            )
            .unwrap();
        assert_eq!(discarded.operation, "discard");
        assert_eq!(discarded.status.generations.len(), 1);
        assert!(producer
            .episode_projection_readiness(AgentId(1))
            .unwrap()
            .is_ready());
    }

    #[test]
    fn generation_rebuild_clock_is_batch_partition_and_restart_independent() {
        let (hippocampus, dir) = temp_hippocampus();
        let event_store = temp_event_store(&dir);
        let mut producer =
            EpisodeProducer::new(hippocampus, &[(1, "Thomas".to_string())], &event_store).unwrap();
        for index in 0..501 {
            append_payload(
                &event_store,
                &DomainEventPayload::BioActionPerformed {
                    agent_id: AgentId(1),
                    action: format!("clock-{index}"),
                },
                if index < 500 { 3600 } else { 0 },
            );
        }
        let active = producer
            .hippocampus()
            .store()
            .load_episode_projection_generation_status()
            .unwrap()
            .active_generation_id;

        assert_eq!(producer.tick(&event_store, 1, 0.001), 500);
        assert_eq!(producer.tick(&event_store, u64::MAX, 99.0), 1);
        let live = producer
            .hippocampus()
            .store()
            .load_episodes("Thomas")
            .unwrap();
        assert_eq!(live.len(), 501);
        assert_eq!(live.last().unwrap().hours_ago, 1.0);
        let (candidate, evidence) = producer
            .build_authoritative_generation_candidate(&event_store, &active)
            .unwrap();
        assert_eq!(candidate.subjects[0].receipts.len(), 501);
        assert_eq!(candidate.subjects[0].live_episodes.len(), 501);
        assert_eq!(evidence.entries[499].effect_reference_tick, 3600);
        assert_eq!(evidence.entries[500].effect_reference_tick, 3600);
        assert_eq!(
            serde_json::to_vec(&live).unwrap(),
            serde_json::to_vec(&candidate.subjects[0].live_episodes).unwrap()
        );

        drop(producer);
        let reopened =
            HippocampusService::open(dir.path().join("test-hippocampus.redb").to_str().unwrap())
                .unwrap();
        let producer =
            EpisodeProducer::new(reopened, &[(1, "Thomas".to_string())], &event_store).unwrap();
        let (rebuilt, rebuilt_evidence) = producer
            .build_authoritative_generation_candidate(&event_store, &active)
            .unwrap();
        assert_eq!(
            serde_json::to_vec(&candidate).unwrap(),
            serde_json::to_vec(&rebuilt).unwrap()
        );
        assert_eq!(evidence, rebuilt_evidence);
    }

    #[test]
    fn generation_rebuild_preserves_archive_and_bounds_live_over_two_thousand_rows() {
        let (hippocampus, dir) = temp_hippocampus();
        let event_store = temp_event_store(&dir);
        let mut producer =
            EpisodeProducer::new(hippocampus, &[(1, "Thomas".to_string())], &event_store).unwrap();
        for index in 0..2001 {
            append_payload(
                &event_store,
                &DomainEventPayload::BioActionPerformed {
                    agent_id: AgentId(1),
                    action: format!("retention-{index}"),
                },
                index,
            );
        }
        while producer.tick(&event_store, 1, 1.0) > 0 {}
        let live = producer
            .hippocampus()
            .store()
            .load_episodes("Thomas")
            .unwrap();
        assert_eq!(live.len(), EPISODE_PROJECTION_MAX_LIVE_EPISODES_PER_SUBJECT);
        producer
            .hippocampus()
            .store()
            .archive_and_clear_episodes("Thomas", &live)
            .unwrap();
        let archive_before = producer
            .hippocampus()
            .store()
            .load_archive("Thomas")
            .unwrap();
        let archive_bytes = serde_json::to_vec(&archive_before).unwrap();
        let active = producer
            .hippocampus()
            .store()
            .load_episode_projection_generation_status()
            .unwrap()
            .active_generation_id;
        let (candidate, evidence) = producer
            .build_authoritative_generation_candidate(&event_store, &active)
            .unwrap();
        let subject = &candidate.subjects[0];
        assert_eq!(subject.receipts.len(), 2001);
        assert_eq!(subject.frontier.applied_count, 2001);
        assert_eq!(
            subject.live_episodes.len(),
            EPISODE_PROJECTION_MAX_LIVE_EPISODES_PER_SUBJECT
        );
        assert_eq!(
            subject.archived_episodes.len(),
            EPISODE_PROJECTION_MAX_LIVE_EPISODES_PER_SUBJECT
        );
        assert!(subject.live_episodes.iter().all(|live_episode| {
            subject
                .archived_episodes
                .iter()
                .all(|archived| archived.id != live_episode.id)
        }));
        let tip = subject.receipts.last().unwrap().episode_id;
        assert!(subject
            .archived_episodes
            .iter()
            .any(|episode| episode.id == tip));

        drop(producer);
        let reopened =
            HippocampusService::open(dir.path().join("test-hippocampus.redb").to_str().unwrap())
                .unwrap();
        let producer =
            EpisodeProducer::new(reopened, &[(1, "Thomas".to_string())], &event_store).unwrap();
        let (rebuilt, rebuilt_evidence) = producer
            .build_authoritative_generation_candidate(&event_store, &active)
            .unwrap();
        assert_eq!(
            serde_json::to_vec(&candidate).unwrap(),
            serde_json::to_vec(&rebuilt).unwrap()
        );
        assert_eq!(evidence, rebuilt_evidence);

        let digest = producer
            .hippocampus()
            .store()
            .begin_episode_projection_generation(&candidate, &active, &evidence)
            .unwrap();
        producer
            .hippocampus()
            .store()
            .validate_episode_projection_generation(
                &candidate.descriptor.generation_id,
                &active,
                &evidence,
            )
            .unwrap();
        producer
            .hippocampus()
            .store()
            .activate_episode_projection_generation(
                &candidate.descriptor.generation_id,
                &active,
                &digest,
                &evidence,
                &evidence,
            )
            .unwrap();
        assert_eq!(
            serde_json::to_vec(
                &producer
                    .hippocampus()
                    .store()
                    .load_archive("Thomas")
                    .unwrap()
            )
            .unwrap(),
            archive_bytes
        );
        assert_eq!(
            producer
                .hippocampus()
                .store()
                .load_episodes("Thomas")
                .unwrap()
                .len(),
            EPISODE_PROJECTION_MAX_LIVE_EPISODES_PER_SUBJECT
        );
    }

    #[test]
    fn generation_activation_rejects_concurrent_archive_change_without_projection_write() {
        let (hippocampus, dir) = temp_hippocampus();
        let event_store = temp_event_store(&dir);
        let producer =
            EpisodeProducer::new(hippocampus, &[(1, "Thomas".to_string())], &event_store).unwrap();
        append_payload(
            &event_store,
            &DomainEventPayload::BioActionPerformed {
                agent_id: AgentId(1),
                action: "archive-cas".to_string(),
            },
            1,
        );
        let active_status = producer
            .hippocampus()
            .store()
            .load_episode_projection_generation_status()
            .unwrap();
        let active = active_status.active_generation_id.clone();
        let (candidate, evidence) = producer
            .build_authoritative_generation_candidate(&event_store, &active)
            .unwrap();
        let mut concurrent_archive = candidate.subjects[0].live_episodes[0].clone();
        concurrent_archive.id = concurrent_archive.id.wrapping_add(1);
        producer
            .hippocampus()
            .store()
            .append_archive("Thomas", std::slice::from_ref(&concurrent_archive))
            .unwrap();
        assert!(producer
            .hippocampus()
            .store()
            .begin_episode_projection_generation(&candidate, &active, &evidence)
            .is_err());
        assert_eq!(
            producer
                .hippocampus()
                .store()
                .load_episode_projection_generation_status()
                .unwrap(),
            active_status
        );
        producer
            .hippocampus()
            .store()
            .store_archive("Thomas", &[])
            .unwrap();
        let digest = producer
            .hippocampus()
            .store()
            .begin_episode_projection_generation(&candidate, &active, &evidence)
            .unwrap();
        producer
            .hippocampus()
            .store()
            .append_archive("Thomas", std::slice::from_ref(&concurrent_archive))
            .unwrap();
        assert!(producer
            .hippocampus()
            .store()
            .validate_episode_projection_generation(
                &candidate.descriptor.generation_id,
                &active,
                &evidence,
            )
            .is_err());
        producer
            .hippocampus()
            .store()
            .store_archive("Thomas", &[])
            .unwrap();
        producer
            .hippocampus()
            .store()
            .validate_episode_projection_generation(
                &candidate.descriptor.generation_id,
                &active,
                &evidence,
            )
            .unwrap();
        let before_status = producer
            .hippocampus()
            .store()
            .load_episode_projection_generation_status()
            .unwrap();
        let before_live = producer
            .hippocampus()
            .store()
            .load_episodes("Thomas")
            .unwrap();
        producer
            .hippocampus()
            .store()
            .append_archive("Thomas", &[concurrent_archive])
            .unwrap();
        let archive_after_external_write = producer
            .hippocampus()
            .store()
            .load_archive("Thomas")
            .unwrap();
        let active_evidence = EpisodeProjectionSourceCutEvidence {
            coverage: active_status
                .generations
                .iter()
                .find(|generation| generation.phase == EpisodeProjectionGenerationPhase::Active)
                .unwrap()
                .snapshot_source_cut
                .clone(),
            entries: Vec::new(),
        };
        assert!(producer
            .hippocampus()
            .store()
            .activate_episode_projection_generation(
                &candidate.descriptor.generation_id,
                &active,
                &digest,
                &evidence,
                &active_evidence,
            )
            .is_err());
        assert_eq!(
            producer
                .hippocampus()
                .store()
                .load_episode_projection_generation_status()
                .unwrap(),
            before_status
        );
        assert_eq!(
            serde_json::to_vec(
                &producer
                    .hippocampus()
                    .store()
                    .load_episodes("Thomas")
                    .unwrap()
            )
            .unwrap(),
            serde_json::to_vec(&before_live).unwrap()
        );
        assert_eq!(
            serde_json::to_vec(
                &producer
                    .hippocampus()
                    .store()
                    .load_archive("Thomas")
                    .unwrap()
            )
            .unwrap(),
            serde_json::to_vec(&archive_after_external_write).unwrap()
        );
    }

    fn cutover_authorization(
        hippocampus: &HippocampusService,
        event_store: &EventStore,
        source_row_id: i64,
        operator_secret: &str,
    ) -> EpisodeProjectionCutoverAuthorization {
        let legacy_state_digest = format!(
            "{:x}",
            Sha256::digest(
                hippocampus
                    .store()
                    .episode_projection_legacy_state_material()
                    .unwrap()
            )
        );
        let source_cut_digest = event_store_source_cut_digest(event_store, source_row_id).unwrap();
        let authorization_digest = cutover_authorization_digest(
            source_row_id,
            &legacy_state_digest,
            &source_cut_digest,
            operator_secret,
        );
        EpisodeProjectionCutoverAuthorization {
            source_row_id,
            legacy_state_digest,
            source_cut_digest,
            authorization_digest,
            operator_secret: operator_secret.to_string(),
        }
    }

    #[test]
    fn test_agent_action_produces_episode() {
        let (hippocampus, dir) = temp_hippocampus();
        let es = temp_event_store(&dir);
        let agents = vec![(1, "Thomas".to_string()), (2, "Lisa".to_string())];
        let producer = EpisodeProducer::new(hippocampus, &agents, &es).unwrap();

        let payload = DomainEventPayload::AgentActionReceived {
            agent_id: AgentId(1),
            action_type: "talk".to_string(),
            content: Some("Wir haben ein Problem mit dem Deadline".to_string()),
            target_room: Some("meetingraum-01".to_string()),
            source: None,
        };

        let result = producer.event_to_episode(&payload, 11, 0.0);
        assert!(result.is_some());

        let (name, episode) = result.unwrap();
        assert_eq!(name, "Thomas");
        assert_eq!(episode.agent_name, "Thomas");
        assert!(episode.summary.contains("Thomas"));
        assert!(episode.summary.contains("talk"));
        // Problem keyword → hohe Relevanz
        assert!(episode.relevance >= 0.7);
        assert_eq!(episode.hours_ago, 0.0);
    }

    #[test]
    fn test_bio_action_produces_episode() {
        let (hippocampus, dir) = temp_hippocampus();
        let es = temp_event_store(&dir);
        let agents = vec![(1, "Thomas".to_string())];
        let producer = EpisodeProducer::new(hippocampus, &agents, &es).unwrap();

        let payload = DomainEventPayload::BioActionPerformed {
            agent_id: AgentId(1),
            action: "eat_meal".to_string(),
        };

        let result = producer.event_to_episode(&payload, 12, 0.0);
        assert!(result.is_some());

        let (name, episode) = result.unwrap();
        assert_eq!(name, "Thomas");
        assert_eq!(episode.relevance, 0.1);
        assert!(episode.tags.contains(&"routine".to_string()));
    }

    #[test]
    fn test_chaos_event_produces_episode() {
        let (hippocampus, dir) = temp_hippocampus();
        let es = temp_event_store(&dir);
        let producer = EpisodeProducer::new(hippocampus, &[], &es).unwrap();

        let payload = DomainEventPayload::ChaosTriggered {
            event_type: sentinel_common::EventType::PrinterBroken,
            target_room: Some("buero-dev-1".to_string()),
            description: "Drucker streikt wieder".to_string(),
            duration_ticks: 0,
        };

        let result = producer.event_to_episode(&payload, 13, 0.0);
        assert!(result.is_some());

        let (name, episode) = result.unwrap();
        assert_eq!(name, "_building");
        assert!(episode.summary.contains("Chaos"));
        assert_eq!(episode.relevance, 0.7);
    }

    fn project_closeout_payload(candidate_digest: &str) -> DomainEventPayload {
        DomainEventPayload::ProjectCloseoutPublished {
            tenant_id: "tenant-a".to_string(),
            project_id: "project-a".to_string(),
            project_generation: 7,
            project_digest: "1111111111111111111111111111111111111111111111111111111111111111"
                .to_string(),
            candidate_id: "candidate-a".to_string(),
            candidate_generation: 3,
            candidate_digest: candidate_digest.to_string(),
            release_id: "release-a".to_string(),
            release_generation: 2,
            release_digest: "2222222222222222222222222222222222222222222222222222222222222222"
                .to_string(),
            acceptance_id: "acceptance-a".to_string(),
            acceptance_generation: 1,
            acceptance_digest: "3333333333333333333333333333333333333333333333333333333333333333"
                .to_string(),
            decisions_digest: "4444444444444444444444444444444444444444444444444444444444444444"
                .to_string(),
            artifact_inventory_digest:
                "5555555555555555555555555555555555555555555555555555555555555555".to_string(),
            failures_digest: "6666666666666666666666666666666666666666666666666666666666666666"
                .to_string(),
            lessons_digest: "7777777777777777777777777777777777777777777777777777777777777777"
                .to_string(),
        }
    }

    #[test]
    fn project_closeout_produces_source_linked_building_episode() {
        let (hippocampus, dir) = temp_hippocampus();
        let event_store = temp_event_store(&dir);
        let producer = EpisodeProducer::new(hippocampus, &[], &event_store).unwrap();
        let candidate_digest = "8888888888888888888888888888888888888888888888888888888888888888";

        let (name, episode) = producer
            .event_to_episode(&project_closeout_payload(candidate_digest), 15, 0.0)
            .expect("valid closeout must enter episodic memory");

        assert_eq!(name, "_building");
        assert_eq!(episode.agent_name, "_building");
        assert_eq!(episode.relevance, 0.95);
        assert!(episode.summary.contains("project-a"));
        assert!(episode.summary.contains("release-a"));
        assert!(episode.tags.contains(&"project_closeout".to_string()));
        assert!(episode
            .tags
            .contains(&"candidate:8888888888888888".to_string()));
        assert!(episode
            .tags
            .contains(&"lessons:7777777777777777".to_string()));
    }

    #[test]
    fn malformed_project_closeout_digest_is_rejected_without_panicking() {
        let (hippocampus, dir) = temp_hippocampus();
        let event_store = temp_event_store(&dir);
        let mut producer = EpisodeProducer::new(hippocampus, &[], &event_store).unwrap();
        let source_row_id = append_payload(&event_store, &project_closeout_payload("short"), 10);

        assert!(producer
            .event_to_episode(&project_closeout_payload("short"), 16, 0.0)
            .is_none());
        assert_eq!(producer.tick(&event_store, 20, 1.0), 0);
        assert_eq!(producer.last_event_id, source_row_id);
        let quarantines = producer
            .hippocampus()
            .store()
            .list_episode_projection_quarantine()
            .unwrap();
        assert_eq!(quarantines.len(), 1);
        assert_eq!(
            quarantines[0].reason,
            EpisodeProjectionQuarantineReason::MalformedRelevantPayload
        );
        assert_eq!(
            quarantines[0].affected_subject,
            Some(EpisodeProjectionSubject::Building)
        );
    }

    #[test]
    fn test_unknown_agent_returns_none() {
        let (hippocampus, dir) = temp_hippocampus();
        let es = temp_event_store(&dir);
        let producer = EpisodeProducer::new(hippocampus, &[], &es).unwrap();

        let payload = DomainEventPayload::AgentActionReceived {
            agent_id: AgentId(99),
            action_type: "talk".to_string(),
            content: None,
            target_room: None,
            source: None,
        };

        let result = producer.event_to_episode(&payload, 14, 0.0);
        assert!(result.is_none(), "Unknown agent should return None");
    }

    #[test]
    fn unknown_agent_without_frontier_is_canonicalized_to_global_quarantine() {
        let (hippocampus, dir) = temp_hippocampus();
        let event_store = temp_event_store(&dir);
        let agents = vec![(1, "Thomas".to_string()), (2, "Lisa".to_string())];
        let mut producer = EpisodeProducer::new(hippocampus, &agents, &event_store).unwrap();
        append_payload(
            &event_store,
            &DomainEventPayload::BioActionPerformed {
                agent_id: AgentId(3),
                action: "drink".to_string(),
            },
            10,
        );

        assert_eq!(producer.tick(&event_store, 20, 1.0), 0);
        let quarantines = producer
            .hippocampus()
            .store()
            .list_episode_projection_quarantine()
            .unwrap();
        assert_eq!(quarantines.len(), 1);
        assert_eq!(quarantines[0].affected_subject, None);
        for agent_id in [AgentId(1), AgentId(2)] {
            let readiness = producer.episode_projection_readiness(agent_id).unwrap();
            assert!(!readiness.is_ready());
            assert!(readiness.blockers.iter().any(|block| matches!(
                block,
                sentinel_hippocampus::EpisodeProjectionReadinessBlock::GlobalQuarantine { .. }
            )));
        }
    }

    #[test]
    fn unknown_runtime_agent_with_durable_frontier_remains_subject_local() {
        let (hippocampus, dir) = temp_hippocampus();
        let event_store = temp_event_store(&dir);
        let agents = vec![(1, "Thomas".to_string()), (2, "Lisa".to_string())];
        let mut producer = EpisodeProducer::new(hippocampus, &agents, &event_store).unwrap();
        producer.agent_names.remove(&2);
        append_payload(
            &event_store,
            &DomainEventPayload::BioActionPerformed {
                agent_id: AgentId(2),
                action: "drink".to_string(),
            },
            10,
        );

        assert_eq!(producer.tick(&event_store, 20, 1.0), 0);
        let quarantines = producer
            .hippocampus()
            .store()
            .list_episode_projection_quarantine()
            .unwrap();
        assert_eq!(
            quarantines[0].affected_subject,
            Some(EpisodeProjectionSubject::Agent {
                agent_id: AgentId(2),
            })
        );
        assert!(producer
            .episode_projection_readiness(AgentId(1))
            .unwrap()
            .is_ready());
        assert!(!producer
            .episode_projection_readiness(AgentId(2))
            .unwrap()
            .is_ready());
    }

    #[test]
    fn test_transit_event_ignored() {
        let (hippocampus, dir) = temp_hippocampus();
        let es = temp_event_store(&dir);
        let producer = EpisodeProducer::new(hippocampus, &[], &es).unwrap();

        let payload = DomainEventPayload::TransitCompleted {
            agent_id: AgentId(1),
            room_id: "kueche".to_string(),
        };

        let result = producer.event_to_episode(&payload, 15, 0.0);
        assert!(result.is_none(), "Transit events should be ignored");
    }

    #[test]
    fn test_classify_conflict_action() {
        let (rel, emo, tags) = classify_action("talk", Some("Wir haben einen Konflikt"));
        assert!(rel >= 0.7, "Conflict should have high relevance: {rel}");
        assert!(emo >= 0.5, "Conflict should have high emotion: {emo}");
        assert!(tags.contains(&"conflict".to_string()));
    }

    #[test]
    fn test_classify_routine_action() {
        let (rel, emo, _tags) = classify_action("eat", None);
        assert!(rel <= 0.2, "Eating should have low relevance: {rel}");
        assert!(emo <= 0.15, "Eating should have low emotion: {emo}");
    }

    #[test]
    fn test_episode_id_is_caller_supplied_stable_identity() {
        let (hippocampus, dir) = temp_hippocampus();
        let es = temp_event_store(&dir);
        let agents = vec![(1, "Thomas".to_string())];
        let producer = EpisodeProducer::new(hippocampus, &agents, &es).unwrap();

        let payload = DomainEventPayload::BioActionPerformed {
            agent_id: AgentId(1),
            action: "drink".to_string(),
        };

        let (_, ep1) = producer.event_to_episode(&payload, 0xfeed, 0.0).unwrap();
        let (_, ep2) = producer.event_to_episode(&payload, 0xfeed, 1.0).unwrap();
        assert_eq!(ep1.id, 0xfeed);
        assert_eq!(ep2.id, 0xfeed);
    }

    #[test]
    fn durable_episode_age_is_independent_of_projection_runtime() {
        let (hippocampus, dir) = temp_hippocampus();
        let es = temp_event_store(&dir);
        let agents = vec![(1, "Thomas".to_string())];
        let producer = EpisodeProducer::new(hippocampus, &agents, &es).unwrap();

        let payload = DomainEventPayload::BioActionPerformed {
            agent_id: AgentId(1),
            action: "eat_meal".to_string(),
        };

        let age = episode_hours_ago(0, 3600, EPISODE_PROJECTION_TICK_DURATION_MILLIS);
        let (_, first) = producer.event_to_episode(&payload, 16, age).unwrap();
        let (_, replayed) = producer.event_to_episode(&payload, 16, age).unwrap();
        assert_eq!(first.hours_ago, 1.0);
        assert_eq!(first.hours_ago, replayed.hours_ago);
    }

    #[test]
    fn test_format_action_summary() {
        let summary = format_action_summary("Thomas", "talk", Some("Hallo Welt"), Some("kueche"));
        assert_eq!(summary, "Thomas: talk in kueche - Hallo Welt");

        let summary = format_action_summary("Lisa", "work", None, None);
        assert_eq!(summary, "Lisa: work");
    }

    #[test]
    fn test_format_action_summary_truncates() {
        let long_content = "A".repeat(100);
        let summary = format_action_summary("Thomas", "talk", Some(&long_content), None);
        assert!(summary.len() < 120, "Summary should be truncated");
        assert!(summary.ends_with("..."));
    }

    #[test]
    fn test_should_run() {
        let (hippocampus, dir) = temp_hippocampus();
        let es = temp_event_store(&dir);
        let producer = EpisodeProducer::new(hippocampus, &[], &es).unwrap();

        assert!(!producer.should_run(0));
        assert!(!producer.should_run(1));
        assert!(!producer.should_run(29));
        assert!(producer.should_run(30));
        assert!(!producer.should_run(31));
        assert!(producer.should_run(60));
    }

    #[test]
    fn test_register_agent() {
        let (hippocampus, dir) = temp_hippocampus();
        let es = temp_event_store(&dir);
        let mut producer = EpisodeProducer::new(hippocampus, &[], &es).unwrap();

        // Vor Registrierung: Agent unbekannt
        let payload = DomainEventPayload::BioActionPerformed {
            agent_id: AgentId(5),
            action: "eat".to_string(),
        };
        assert!(producer.event_to_episode(&payload, 18, 0.0).is_none());

        // Nach Registrierung: Agent bekannt
        producer.register_agent(5, "Kevin".to_string()).unwrap();
        let result = producer.event_to_episode(&payload, 18, 0.0);
        assert!(result.is_some());
        assert_eq!(result.unwrap().0, "Kevin");
    }

    #[test]
    fn episode_projection_staged_agent_bindings_register_restart_idempotently() {
        let (hippocampus, dir) = temp_hippocampus();
        let event_store = temp_event_store(&dir);
        let existing = vec![(1, "Thomas".to_string())];
        let mut producer = EpisodeProducer::new(hippocampus, &existing, &event_store).unwrap();
        let staged = vec![(1, "Thomas".to_string()), (2, "Kevin".to_string())];

        producer.validate_agent_bindings(&staged).unwrap();
        assert!(producer
            .hippocampus()
            .store()
            .load_episode_projection_frontier(EpisodeProjectionSubject::Agent {
                agent_id: AgentId(2),
            })
            .unwrap()
            .is_none());
        producer.register_agents(&staged).unwrap();
        assert!(producer
            .episode_projection_readiness(AgentId(2))
            .unwrap()
            .is_ready());

        drop(producer);
        let hippocampus =
            HippocampusService::open(dir.path().join("test-hippocampus.redb").to_str().unwrap())
                .unwrap();
        let restarted = EpisodeProducer::new(hippocampus, &staged, &event_store).unwrap();
        restarted.validate_agent_bindings(&staged).unwrap();
        assert!(restarted
            .episode_projection_readiness(AgentId(2))
            .unwrap()
            .is_ready());
    }

    #[test]
    fn episode_projection_staged_agent_binding_rejects_rename_and_alias_without_writes() {
        let (hippocampus, dir) = temp_hippocampus();
        let event_store = temp_event_store(&dir);
        let existing = vec![(1, "Thomas".to_string()), (2, "Kevin".to_string())];
        let producer = EpisodeProducer::new(hippocampus, &existing, &event_store).unwrap();
        let before_one = producer
            .hippocampus()
            .store()
            .load_episode_projection_frontier(EpisodeProjectionSubject::Agent {
                agent_id: AgentId(1),
            })
            .unwrap()
            .unwrap();
        let before_two = producer
            .hippocampus()
            .store()
            .load_episode_projection_frontier(EpisodeProjectionSubject::Agent {
                agent_id: AgentId(2),
            })
            .unwrap()
            .unwrap();

        let rename = producer
            .validate_agent_bindings(&[(1, "Renamed".to_string()), (2, "Kevin".to_string())])
            .unwrap_err();
        assert!(format!("{rename:#}").contains("bucket name is immutable"));
        let alias = producer
            .validate_agent_bindings(&[(1, "Thomas".to_string()), (3, "Kevin".to_string())])
            .unwrap_err();
        assert!(format!("{alias:#}").contains("already bound"));
        assert_eq!(
            producer
                .hippocampus()
                .store()
                .load_episode_projection_frontier(EpisodeProjectionSubject::Agent {
                    agent_id: AgentId(1),
                })
                .unwrap()
                .unwrap(),
            before_one
        );
        assert_eq!(
            producer
                .hippocampus()
                .store()
                .load_episode_projection_frontier(EpisodeProjectionSubject::Agent {
                    agent_id: AgentId(2),
                })
                .unwrap()
                .unwrap(),
            before_two
        );
        assert!(producer
            .hippocampus()
            .store()
            .load_episode_projection_frontier(EpisodeProjectionSubject::Agent {
                agent_id: AgentId(3),
            })
            .unwrap()
            .is_none());
    }

    #[test]
    fn beginning_policy_processes_events_that_predate_first_start() {
        let (hippocampus, dir) = temp_hippocampus();
        let event_store = temp_event_store(&dir);
        let payload = DomainEventPayload::BioActionPerformed {
            agent_id: AgentId(1),
            action: "drink".to_string(),
        };
        let source_row_id = append_payload(&event_store, &payload, 10);
        let agents = vec![(1, "Thomas".to_string())];
        let mut producer = EpisodeProducer::new(hippocampus, &agents, &event_store).unwrap();

        assert_eq!(producer.last_event_id, 0);
        assert_eq!(producer.tick(&event_store, 20, 1.0), 1);
        assert_eq!(producer.last_event_id, source_row_id);
        assert_eq!(
            event_store.get_offset(OFFSET_NAME).unwrap(),
            Some(source_row_id)
        );
        let control = producer
            .hippocampus()
            .store()
            .load_episode_projection_control()
            .unwrap()
            .unwrap();
        assert_eq!(
            control.start_policy,
            EpisodeProjectionStartPolicy::Beginning
        );
        assert_eq!(control.last_source_row_id, source_row_id);
        assert_eq!(
            producer
                .hippocampus()
                .store()
                .load_episodes("Thomas")
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn beginning_policy_refuses_legacy_episodes_without_changing_mirror_or_state() {
        let (hippocampus, dir) = temp_hippocampus();
        hippocampus
            .store()
            .store_episodes(
                "Thomas",
                &[Episode {
                    id: 77,
                    agent_name: "Thomas".to_string(),
                    summary: "Legacy episode".to_string(),
                    relevance: 0.5,
                    emotion: 0.2,
                    repetitions: 1,
                    hours_ago: 1.0,
                    participants: Vec::new(),
                    tags: vec!["legacy".to_string()],
                }],
            )
            .unwrap();
        let event_store = temp_event_store(&dir);
        let old_row = append_payload(
            &event_store,
            &DomainEventPayload::TransitCompleted {
                agent_id: AgentId(1),
                room_id: "lobby".to_string(),
            },
            5,
        );
        event_store.update_offset(OFFSET_NAME, old_row).unwrap();
        let new_event = DomainEvent::new(
            "bio_action_performed",
            "AGENT-01",
            &DomainEventPayload::BioActionPerformed {
                agent_id: AgentId(1),
                action: "drink".to_string(),
            }
            .to_json(),
            "episode-producer-test",
            10,
        );
        event_store.append_event(&new_event).unwrap();

        let agents = vec![(1, "Thomas".to_string())];
        let error = EpisodeProducer::new(hippocampus, &agents, &event_store)
            .err()
            .unwrap();
        assert!(error
            .to_string()
            .contains("Beginning episode projection requires an empty legacy episode store"));
        assert_eq!(event_store.get_offset(OFFSET_NAME).unwrap(), Some(old_row));
        let reopened =
            HippocampusService::open(dir.path().join("test-hippocampus.redb").to_str().unwrap())
                .unwrap();
        let episodes = reopened.store().load_episodes("Thomas").unwrap();
        assert_eq!(episodes.len(), 1);
        assert_eq!(episodes[0].id, 77);
        assert!(reopened
            .store()
            .load_episode_projection_control()
            .unwrap()
            .is_none());
        let subject = EpisodeProjectionSubject::Agent {
            agent_id: AgentId(1),
        };
        assert!(reopened
            .store()
            .load_episode_projection_frontier(subject)
            .unwrap()
            .is_none());
        assert!(reopened
            .store()
            .load_episode_source_receipt(subject, &new_event.event_id)
            .unwrap()
            .is_none());
        let readiness = reopened
            .store()
            .load_episode_projection_readiness(subject)
            .unwrap();
        assert!(!readiness.is_ready());
        assert!(readiness.blockers.iter().any(|block| matches!(
            block,
            sentinel_hippocampus::EpisodeProjectionReadinessBlock::ProjectionUninitialized
        )));
    }

    #[test]
    fn sealed_cutover_binds_legacy_state_and_source_cut_and_restarts() {
        let (hippocampus, dir) = temp_hippocampus();
        hippocampus
            .store()
            .store_episodes(
                "Thomas",
                &[Episode {
                    id: 77,
                    agent_name: "Thomas".to_string(),
                    summary: "Legacy episode".to_string(),
                    relevance: 0.5,
                    emotion: 0.2,
                    repetitions: 1,
                    hours_ago: 1.0,
                    participants: Vec::new(),
                    tags: vec!["legacy".to_string()],
                }],
            )
            .unwrap();
        let event_store = temp_event_store(&dir);
        let old_row = append_payload(
            &event_store,
            &DomainEventPayload::TransitCompleted {
                agent_id: AgentId(1),
                room_id: "lobby".to_string(),
            },
            5,
        );
        event_store.update_offset(OFFSET_NAME, old_row).unwrap();
        append_payload(
            &event_store,
            &DomainEventPayload::BioActionPerformed {
                agent_id: AgentId(1),
                action: "drink".to_string(),
            },
            10,
        );

        let agents = vec![(1, "Thomas".to_string())];
        let legacy_state_digest = format!(
            "{:x}",
            Sha256::digest(
                hippocampus
                    .store()
                    .episode_projection_legacy_state_material()
                    .unwrap()
            )
        );
        let source_cut_digest = event_store_source_cut_digest(&event_store, old_row).unwrap();
        let operator_secret = "s".repeat(32);
        let authorization_digest = cutover_authorization_digest(
            old_row,
            &legacy_state_digest,
            &source_cut_digest,
            &operator_secret,
        );
        let mut producer = EpisodeProducer::new_with_cutover_authorization(
            hippocampus,
            &agents,
            &event_store,
            EpisodeProjectionCutoverAuthorization {
                source_row_id: old_row,
                legacy_state_digest: legacy_state_digest.clone(),
                source_cut_digest: source_cut_digest.clone(),
                authorization_digest: authorization_digest.clone(),
                operator_secret,
            },
        )
        .unwrap();
        assert_eq!(producer.last_event_id, old_row);
        assert_eq!(producer.tick(&event_store, 20, 1.0), 1);
        let episodes = producer
            .hippocampus()
            .store()
            .load_episodes("Thomas")
            .unwrap();
        assert_eq!(episodes.len(), 2);
        assert_eq!(episodes[0].id, 77);
        assert_eq!(
            producer
                .hippocampus()
                .store()
                .load_episode_projection_control()
                .unwrap()
                .unwrap()
                .start_policy,
            EpisodeProjectionStartPolicy::RecoveryCut {
                source_row_id: old_row,
                proof_digest: authorization_digest.clone(),
            }
        );
        let receipt = producer
            .hippocampus()
            .store()
            .load_episode_projection_cutover_receipt()
            .unwrap()
            .unwrap();
        assert_eq!(receipt.legacy_state_digest, legacy_state_digest);
        assert_eq!(receipt.source_cut_digest, source_cut_digest);

        let hippocampus_path = dir.path().join("test-hippocampus.redb");
        drop(producer);
        let reopened = HippocampusService::open(hippocampus_path.to_str().unwrap()).unwrap();
        let restarted = EpisodeProducer::new(reopened, &agents, &event_store).unwrap();
        assert_eq!(restarted.last_event_id, old_row + 1);
    }

    #[test]
    fn sealed_cutover_rejects_state_source_and_authentication_rebinding() {
        for mutation in ["legacy", "source", "authorization"] {
            let (hippocampus, dir) = temp_hippocampus();
            hippocampus
                .store()
                .store_episodes(
                    "Thomas",
                    &[Episode {
                        id: 77,
                        agent_name: "Thomas".to_string(),
                        summary: "Legacy episode".to_string(),
                        relevance: 0.5,
                        emotion: 0.2,
                        repetitions: 1,
                        hours_ago: 1.0,
                        participants: Vec::new(),
                        tags: vec!["legacy".to_string()],
                    }],
                )
                .unwrap();
            let event_store = temp_event_store(&dir);
            let source_row_id = append_payload(
                &event_store,
                &DomainEventPayload::TransitCompleted {
                    agent_id: AgentId(1),
                    room_id: "lobby".to_string(),
                },
                5,
            );
            event_store
                .update_offset(OFFSET_NAME, source_row_id)
                .unwrap();
            let mut authorization =
                cutover_authorization(&hippocampus, &event_store, source_row_id, &"s".repeat(32));
            match mutation {
                "legacy" => authorization.legacy_state_digest = "00".repeat(32),
                "source" => authorization.source_cut_digest = "11".repeat(32),
                "authorization" => authorization.authorization_digest = "22".repeat(32),
                _ => unreachable!(),
            }

            let error = EpisodeProducer::new_with_cutover_authorization(
                hippocampus,
                &[(1, "Thomas".to_string())],
                &event_store,
                authorization,
            )
            .err()
            .unwrap();
            assert!(error.to_string().contains("cutover"));
            assert_eq!(
                event_store.get_offset(OFFSET_NAME).unwrap(),
                Some(source_row_id)
            );
            let reopened = HippocampusService::open(
                dir.path().join("test-hippocampus.redb").to_str().unwrap(),
            )
            .unwrap();
            assert!(reopened
                .store()
                .load_episode_projection_control()
                .unwrap()
                .is_none());
            assert!(reopened
                .store()
                .load_episode_projection_cutover_receipt()
                .unwrap()
                .is_none());
            assert_eq!(reopened.store().load_episodes("Thomas").unwrap().len(), 1);
        }
    }

    #[test]
    fn operator_resolution_is_cas_bound_ordered_and_reopens_subject_readiness() {
        let (hippocampus, dir) = temp_hippocampus();
        let event_store = temp_event_store(&dir);
        let agents = vec![(1, "Thomas".to_string()), (2, "Lisa".to_string())];
        let mut producer = EpisodeProducer::new(hippocampus, &agents, &event_store).unwrap();
        producer.agent_names.remove(&2);
        let first_row = append_payload(
            &event_store,
            &DomainEventPayload::BioActionPerformed {
                agent_id: AgentId(2),
                action: "drink".to_string(),
            },
            10,
        );
        let second_row = append_payload(
            &event_store,
            &DomainEventPayload::BioActionPerformed {
                agent_id: AgentId(2),
                action: "eat".to_string(),
            },
            11,
        );
        assert_eq!(producer.tick(&event_store, 20, 1.0), 0);
        let quarantines = producer
            .hippocampus()
            .store()
            .list_episode_projection_quarantine()
            .unwrap();
        assert_eq!(quarantines.len(), 2);
        assert_eq!(quarantines[0].source_row_id, first_row);
        assert_eq!(quarantines[1].source_row_id, second_row);
        assert_eq!(
            quarantines[1].reason,
            EpisodeProjectionQuarantineReason::BlockedByEarlierQuarantine
        );
        assert!(producer.admission_snapshot().allows_agent(AgentId(1)));
        assert!(!producer.admission_snapshot().allows_agent(AgentId(2)));
        producer.register_agent(2, "Lisa".to_string()).unwrap();

        let request_for =
            |quarantine: &EpisodeProjectionQuarantine| EpisodeProjectionResolveRequest {
                source_row_id: quarantine.source_row_id,
                source_event_id: quarantine.source_event_id.clone(),
                request_digest: quarantine.request_digest.clone(),
                quarantine_digest: quarantine_record_digest(quarantine),
            };
        let error = producer
            .resolve_quarantine(&event_store, 20, 1.0, &request_for(&quarantines[1]))
            .unwrap_err();
        assert!(error.to_string().contains("source order"));
        let mut stale = request_for(&quarantines[0]);
        stale.quarantine_digest = "ff".repeat(32);
        let error = producer
            .resolve_quarantine(&event_store, 20, 1.0, &stale)
            .unwrap_err();
        assert!(error.to_string().contains("digest CAS conflict"));

        let first = producer
            .resolve_quarantine(&event_store, 20, 1.0, &request_for(&quarantines[0]))
            .unwrap();
        assert!(first.resolved);
        assert!(!first.duplicate);
        assert!(!producer.admission_snapshot().allows_agent(AgentId(2)));
        let second_request = request_for(&quarantines[1]);
        let second = producer
            .resolve_quarantine(&event_store, 20, 1.0, &second_request)
            .unwrap();
        assert!(second.resolved);
        assert!(!second.duplicate);
        assert!(producer.admission_snapshot().allows_agent(AgentId(2)));
        assert_eq!(
            producer
                .hippocampus()
                .store()
                .load_episodes("Lisa")
                .unwrap()
                .len(),
            2
        );

        let duplicate = producer
            .resolve_quarantine(&event_store, 20, 1.0, &second_request)
            .unwrap();
        assert!(duplicate.duplicate);
        assert_eq!(
            producer
                .hippocampus()
                .store()
                .load_episodes("Lisa")
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn malformed_relevant_event_is_quarantined_without_agent_advance() {
        let (hippocampus, dir) = temp_hippocampus();
        let event_store = temp_event_store(&dir);
        let malformed = DomainEvent::new(
            "agent_action_received",
            "AGENT-01",
            "{not-json",
            "episode-producer-test",
            10,
        );
        let source_row_id = event_store.append_event(&malformed).unwrap();
        let agents = vec![(1, "Thomas".to_string())];
        let mut producer = EpisodeProducer::new(hippocampus, &agents, &event_store).unwrap();

        assert_eq!(producer.tick(&event_store, 20, 1.0), 0);
        assert_eq!(producer.last_event_id, source_row_id);
        assert_eq!(
            event_store.get_offset(OFFSET_NAME).unwrap(),
            Some(source_row_id)
        );
        let quarantines = producer
            .hippocampus()
            .store()
            .list_episode_projection_quarantine()
            .unwrap();
        assert_eq!(quarantines.len(), 1);
        assert_eq!(
            quarantines[0].reason,
            EpisodeProjectionQuarantineReason::MalformedRelevantPayload
        );
        let encoded_quarantine = serde_json::to_value(&quarantines[0]).unwrap();
        let quarantine_fields = encoded_quarantine.as_object().unwrap();
        assert!(!quarantine_fields.contains_key("payload"));
        assert!(!quarantine_fields.contains_key("diagnostic"));
        assert!(quarantine_fields.contains_key("diagnostic_digest"));
        assert!(!encoded_quarantine.to_string().contains("{not-json"));
        assert_eq!(
            producer
                .hippocampus()
                .store()
                .load_episode_projection_frontier(EpisodeProjectionSubject::Agent {
                    agent_id: AgentId(1),
                })
                .unwrap()
                .unwrap()
                .last_source_row_id,
            0
        );
        let readiness = producer.episode_projection_readiness(AgentId(1)).unwrap();
        assert!(!readiness.is_ready());
        assert!(readiness.blockers.iter().any(|block| matches!(
            block,
            sentinel_hippocampus::EpisodeProjectionReadinessBlock::SubjectQuarantine { .. }
        )));
    }

    #[test]
    fn malformed_event_without_resolvable_subject_blocks_globally() {
        let (hippocampus, dir) = temp_hippocampus();
        let event_store = temp_event_store(&dir);
        let malformed = DomainEvent::new(
            "agent_action_received",
            "unresolved-agent",
            "{not-json",
            "episode-producer-test",
            10,
        );
        event_store.append_event(&malformed).unwrap();
        let agents = vec![(1, "Thomas".to_string()), (2, "Lisa".to_string())];
        let mut producer = EpisodeProducer::new(hippocampus, &agents, &event_store).unwrap();

        assert_eq!(producer.tick(&event_store, 20, 1.0), 0);
        let quarantines = producer
            .hippocampus()
            .store()
            .list_episode_projection_quarantine()
            .unwrap();
        assert_eq!(quarantines[0].affected_subject, None);
        for agent_id in [AgentId(1), AgentId(2)] {
            let readiness = producer.episode_projection_readiness(agent_id).unwrap();
            assert!(!readiness.is_ready());
            assert!(readiness.blockers.iter().any(|block| matches!(
                block,
                sentinel_hippocampus::EpisodeProjectionReadinessBlock::GlobalQuarantine { .. }
            )));
        }
    }

    #[test]
    fn restart_reconciles_limbo_mirror_from_hippocampus_frontier() {
        let dir = tempfile::tempdir().unwrap();
        let hippocampus_path = dir.path().join("restart-hippocampus.redb");
        let event_store = temp_event_store(&dir);
        let payload = DomainEventPayload::BioActionPerformed {
            agent_id: AgentId(1),
            action: "eat".to_string(),
        };
        let source_row_id = append_payload(&event_store, &payload, 10);
        let agents = vec![(1, "Thomas".to_string())];

        {
            let hippocampus = HippocampusService::open(hippocampus_path.to_str().unwrap()).unwrap();
            let mut producer = EpisodeProducer::new(hippocampus, &agents, &event_store).unwrap();
            assert_eq!(producer.tick(&event_store, 20, 1.0), 1);
        }
        event_store
            .force_reset_offset(OFFSET_NAME, source_row_id + 100)
            .unwrap();

        let hippocampus = HippocampusService::open(hippocampus_path.to_str().unwrap()).unwrap();
        let mut restarted = EpisodeProducer::new(hippocampus, &agents, &event_store).unwrap();
        assert_eq!(restarted.last_event_id, source_row_id);
        assert_eq!(
            event_store.get_offset(OFFSET_NAME).unwrap(),
            Some(source_row_id)
        );
        assert_eq!(restarted.tick(&event_store, 30, 1.0), 0);
        assert_eq!(
            restarted
                .hippocampus()
                .store()
                .load_episodes("Thomas")
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn stable_episode_identity_and_request_digest_are_deterministic() {
        let event = DomainEvent::new(
            "bio_action_performed",
            "AGENT-01",
            "{\"type\":\"BioActionPerformed\",\"agent_id\":1,\"action\":\"eat\"}",
            "correlation",
            10,
        );
        let digest = source_request_digest(&event);
        assert_eq!(digest, source_request_digest(&event));
        let thomas = EpisodeProjectionSubject::Agent {
            agent_id: AgentId(1),
        };
        let lisa = EpisodeProjectionSubject::Agent {
            agent_id: AgentId(2),
        };
        let id = stable_episode_id(thomas, &event.event_id, 1, &digest);
        assert_eq!(id, stable_episode_id(thomas, &event.event_id, 1, &digest));
        assert_ne!(id, stable_episode_id(lisa, &event.event_id, 1, &digest));
        assert_ne!(
            id,
            stable_episode_id(
                EpisodeProjectionSubject::Building,
                &event.event_id,
                1,
                &digest
            )
        );

        let mut changed = event.clone();
        changed.tick += 1;
        assert_ne!(digest, source_request_digest(&changed));
    }
}
