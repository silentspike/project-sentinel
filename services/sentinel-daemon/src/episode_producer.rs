//! Episode Producer — Konvertiert DomainEvents aus Limbo zu Hippocampus-Episoden.
//!
//! Laeuft periodisch im ECS Tick-Loop (alle N Ticks), liest neue Events
//! aus dem Limbo EventStore via Cursor und erzeugt Episode-Objekte fuer
//! den HippocampusService. Nightrun konsolidiert diese spaeter.

use std::collections::HashMap;

use sentinel_common::events::{DomainEvent, DomainEventPayload};
use sentinel_hippocampus::{
    Episode, EpisodeProjectionAdvance, EpisodeProjectionApplyOutcome, EpisodeProjectionControl,
    EpisodeProjectionQuarantine, EpisodeProjectionQuarantineReason, EpisodeProjectionStartPolicy,
    EpisodeProjectionWrite, HippocampusService, EPISODE_PROJECTION_VERSION,
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
    /// Whether the durable projection contract was initialized successfully.
    projection_initialized: bool,
    /// Zaehler fuer aufeinanderfolgende Laeufe ohne konvertierbare Events (Starvation-Diagnostik).
    empty_runs: u32,
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
    ) -> Self {
        let mut producer = Self {
            hippocampus,
            last_event_id: 0,
            agent_names: agents.iter().cloned().collect(),
            projection_initialized: false,
            empty_runs: 0,
        };
        producer.ensure_projection_initialized(event_store);
        producer
    }

    /// Gibt eine Referenz auf den HippocampusService zurueck.
    pub fn hippocampus(&self) -> &HippocampusService {
        &self.hippocampus
    }

    /// Registriert einen neuen Agenten (z.B. bei Schichtwechsel).
    pub fn register_agent(&mut self, id: u16, name: String) {
        match self
            .hippocampus
            .store()
            .initialize_episode_projection_agent(&name)
        {
            Ok(_) => {
                self.agent_names.insert(id, name);
            }
            Err(error) => {
                warn!(agent_id = id, agent = %name, %error, "Episode Producer: Agent-Frontier konnte nicht initialisiert werden");
            }
        }
    }

    /// Ob dieser Tick ein Produktionslauf sein soll.
    pub fn should_run(&self, tick: u64) -> bool {
        tick > 0 && tick.is_multiple_of(PRODUCE_INTERVAL_TICKS)
    }

    /// Verarbeitet neue Events aus Limbo und erzeugt Episoden.
    ///
    /// Gibt die Anzahl produzierter Episoden zurueck.
    pub fn tick(&mut self, event_store: &EventStore, current_tick: u64, tick_rate_s: f64) -> usize {
        if !self.projection_initialized && !self.ensure_projection_initialized(event_store) {
            return 0;
        }

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

        let mut total = 0;
        let mut agents_with_episodes = std::collections::HashSet::new();

        for (source_row_id, event) in &events {
            let request_digest = source_request_digest(event);

            if !is_episode_event_type(&event.event_type) {
                let advance = EpisodeProjectionAdvance {
                    source_event_id: event.event_id.clone(),
                    source_row_id: *source_row_id,
                    projection_version: EPISODE_PROJECTION_VERSION,
                    request_digest,
                    expected_global_frontier: self.last_event_id,
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

            let payload: DomainEventPayload = match serde_json::from_str(&event.payload) {
                Ok(payload) => payload,
                Err(error) => {
                    if !self.quarantine_event(
                        event_store,
                        *source_row_id,
                        event,
                        request_digest,
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
                    request_digest,
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

            let Some(stable_agent_name) = self.episode_agent_name(&payload) else {
                if !self.quarantine_event(
                    event_store,
                    *source_row_id,
                    event,
                    request_digest,
                    EpisodeProjectionQuarantineReason::UnknownAgent,
                    "relevant event references an unregistered agent",
                ) {
                    break;
                }
                continue;
            };
            let episode_id = stable_episode_id(
                &stable_agent_name,
                &event.event_id,
                EPISODE_PROJECTION_VERSION,
                &request_digest,
            );
            let Some((agent_name, episode)) =
                self.event_to_episode(&payload, episode_id, event.tick, current_tick, tick_rate_s)
            else {
                warn!(event_id = %event.event_id, "Episode Producer: validiertes relevantes Event wurde nicht konvertiert");
                break;
            };
            debug_assert_eq!(agent_name, stable_agent_name);

            let input = EpisodeProjectionWrite {
                agent_name: agent_name.clone(),
                source_event_id: event.event_id.clone(),
                source_row_id: *source_row_id,
                projection_version: EPISODE_PROJECTION_VERSION,
                request_digest,
                expected_global_frontier: self.last_event_id,
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

    fn ensure_projection_initialized(&mut self, event_store: &EventStore) -> bool {
        let mut agents: Vec<String> = self.agent_names.values().cloned().collect();
        agents.push("_building".to_string());
        agents.sort();
        agents.dedup();

        match self
            .hippocampus
            .store()
            .initialize_episode_projection(&EpisodeProjectionStartPolicy::Beginning, &agents)
        {
            Ok(control) => {
                self.projection_initialized = true;
                self.commit_source_cursor(event_store, &control);
                true
            }
            Err(error) => {
                self.projection_initialized = false;
                warn!(%error, "Episode Producer: durable Projection konnte nicht initialisiert werden");
                false
            }
        }
    }

    fn commit_source_cursor(
        &mut self,
        event_store: &EventStore,
        control: &EpisodeProjectionControl,
    ) {
        self.last_event_id = control.last_source_row_id;
        let mirror = event_store.get_offset(OFFSET_NAME);
        let result = match mirror {
            Ok(Some(current)) if current > self.last_event_id => {
                event_store.force_reset_offset(OFFSET_NAME, self.last_event_id)
            }
            Ok(_) => event_store.update_offset(OFFSET_NAME, self.last_event_id),
            Err(error) => {
                warn!(%error, "Episode Producer: Limbo-Mirror konnte nicht gelesen werden");
                return;
            }
        };
        if let Err(error) = result {
            warn!(cursor = self.last_event_id, %error, "Episode Producer: Limbo-Mirror konnte nicht reconciled werden");
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn quarantine_event(
        &mut self,
        event_store: &EventStore,
        source_row_id: i64,
        event: &DomainEvent,
        request_digest: String,
        reason: EpisodeProjectionQuarantineReason,
        diagnostic: &str,
    ) -> bool {
        let record = EpisodeProjectionQuarantine {
            affected_scope: quarantine_scope(event),
            source_event_id: event.event_id.clone(),
            source_row_id,
            event_type: event.event_type.clone(),
            projection_version: EPISODE_PROJECTION_VERSION,
            request_digest,
            reason,
            diagnostic: bounded_diagnostic(diagnostic),
        };
        match self
            .hippocampus
            .store()
            .quarantine_episode_projection(&record, self.last_event_id)
        {
            Ok(control) => {
                self.commit_source_cursor(event_store, &control);
                true
            }
            Err(error) => {
                warn!(source_row_id, event_id = %event.event_id, %error, "Episode Producer: Quarantaene-Commit fehlgeschlagen");
                false
            }
        }
    }

    fn episode_agent_name(&self, payload: &DomainEventPayload) -> Option<String> {
        match payload {
            DomainEventPayload::AgentActionReceived { agent_id, .. }
            | DomainEventPayload::BioActionPerformed { agent_id, .. } => {
                self.agent_names.get(&agent_id.0).cloned()
            }
            DomainEventPayload::ChaosTriggered { .. } => Some("_building".to_string()),
            _ => None,
        }
    }

    /// Konvertiert einen DomainEventPayload in eine Episode (wenn relevant).
    fn event_to_episode(
        &self,
        payload: &DomainEventPayload,
        episode_id: u64,
        event_tick: u64,
        current_tick: u64,
        tick_rate_s: f64,
    ) -> Option<(String, Episode)> {
        let hours_ago = (current_tick.saturating_sub(event_tick) as f64 * tick_rate_s) / 3600.0;

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

            // Andere Event-Typen sind nicht episoden-relevant
            _ => None,
        }
    }
}

fn is_episode_event_type(event_type: &str) -> bool {
    matches!(
        event_type,
        "agent_action_received" | "bio_action_performed" | "chaos_triggered"
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

fn stable_episode_id(
    agent_scope: &str,
    source_event_id: &str,
    projection_version: u32,
    request_digest: &str,
) -> u64 {
    let mut digest = Sha256::new();
    digest.update(b"sentinel-episode-id-v1\0");
    digest_field(&mut digest, agent_scope.as_bytes());
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

fn quarantine_scope(event: &DomainEvent) -> String {
    if !event.aggregate_id.is_empty()
        && event.aggregate_id.len() <= 512
        && !event.aggregate_id.contains('\u{1f}')
    {
        return event.aggregate_id.clone();
    }
    let mut digest = Sha256::new();
    digest.update(b"sentinel-episode-quarantine-scope-v1\0");
    digest_field(&mut digest, event.aggregate_id.as_bytes());
    format!("unresolved:{:x}", digest.finalize())
}

fn bounded_diagnostic(value: &str) -> String {
    if value.len() <= 256 {
        return value.to_string();
    }
    let truncate_at = value
        .char_indices()
        .take_while(|(index, _)| *index <= 256)
        .last()
        .map(|(index, _)| index)
        .unwrap_or(0);
    value[..truncate_at].to_string()
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
    use sentinel_common::AgentId;

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
    fn test_agent_action_produces_episode() {
        let (hippocampus, dir) = temp_hippocampus();
        let es = temp_event_store(&dir);
        let agents = vec![(1, "Thomas".to_string()), (2, "Lisa".to_string())];
        let producer = EpisodeProducer::new(hippocampus, &agents, &es);

        let payload = DomainEventPayload::AgentActionReceived {
            agent_id: AgentId(1),
            action_type: "talk".to_string(),
            content: Some("Wir haben ein Problem mit dem Deadline".to_string()),
            target_room: Some("meetingraum-01".to_string()),
            source: None,
        };

        let result = producer.event_to_episode(&payload, 11, 100, 200, 1.0);
        assert!(result.is_some());

        let (name, episode) = result.unwrap();
        assert_eq!(name, "Thomas");
        assert_eq!(episode.agent_name, "Thomas");
        assert!(episode.summary.contains("Thomas"));
        assert!(episode.summary.contains("talk"));
        // Problem keyword → hohe Relevanz
        assert!(episode.relevance >= 0.7);
        // hours_ago = (200-100) * 1.0 / 3600 ≈ 0.028
        assert!(episode.hours_ago < 0.03);
    }

    #[test]
    fn test_bio_action_produces_episode() {
        let (hippocampus, dir) = temp_hippocampus();
        let es = temp_event_store(&dir);
        let agents = vec![(1, "Thomas".to_string())];
        let producer = EpisodeProducer::new(hippocampus, &agents, &es);

        let payload = DomainEventPayload::BioActionPerformed {
            agent_id: AgentId(1),
            action: "eat_meal".to_string(),
        };

        let result = producer.event_to_episode(&payload, 12, 50, 100, 1.0);
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
        let producer = EpisodeProducer::new(hippocampus, &[], &es);

        let payload = DomainEventPayload::ChaosTriggered {
            event_type: sentinel_common::EventType::PrinterBroken,
            target_room: Some("buero-dev-1".to_string()),
            description: "Drucker streikt wieder".to_string(),
            duration_ticks: 0,
        };

        let result = producer.event_to_episode(&payload, 13, 0, 100, 1.0);
        assert!(result.is_some());

        let (name, episode) = result.unwrap();
        assert_eq!(name, "_building");
        assert!(episode.summary.contains("Chaos"));
        assert_eq!(episode.relevance, 0.7);
    }

    #[test]
    fn test_unknown_agent_returns_none() {
        let (hippocampus, dir) = temp_hippocampus();
        let es = temp_event_store(&dir);
        let producer = EpisodeProducer::new(hippocampus, &[], &es);

        let payload = DomainEventPayload::AgentActionReceived {
            agent_id: AgentId(99),
            action_type: "talk".to_string(),
            content: None,
            target_room: None,
            source: None,
        };

        let result = producer.event_to_episode(&payload, 14, 0, 100, 1.0);
        assert!(result.is_none(), "Unknown agent should return None");
    }

    #[test]
    fn test_transit_event_ignored() {
        let (hippocampus, dir) = temp_hippocampus();
        let es = temp_event_store(&dir);
        let producer = EpisodeProducer::new(hippocampus, &[], &es);

        let payload = DomainEventPayload::TransitCompleted {
            agent_id: AgentId(1),
            room_id: "kueche".to_string(),
        };

        let result = producer.event_to_episode(&payload, 15, 0, 100, 1.0);
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
        let producer = EpisodeProducer::new(hippocampus, &agents, &es);

        let payload = DomainEventPayload::BioActionPerformed {
            agent_id: AgentId(1),
            action: "drink".to_string(),
        };

        let (_, ep1) = producer
            .event_to_episode(&payload, 0xfeed, 0, 10, 1.0)
            .unwrap();
        let (_, ep2) = producer
            .event_to_episode(&payload, 0xfeed, 5, 10, 1.0)
            .unwrap();
        assert_eq!(ep1.id, 0xfeed);
        assert_eq!(ep2.id, 0xfeed);
    }

    #[test]
    fn test_hours_ago_calculation() {
        let (hippocampus, dir) = temp_hippocampus();
        let es = temp_event_store(&dir);
        let agents = vec![(1, "Thomas".to_string())];
        let producer = EpisodeProducer::new(hippocampus, &agents, &es);

        let payload = DomainEventPayload::BioActionPerformed {
            agent_id: AgentId(1),
            action: "eat_meal".to_string(),
        };

        // Event bei Tick 0, aktuell Tick 3600 (= 1 Stunde bei 1s Tick-Rate)
        let (_, episode) = producer
            .event_to_episode(&payload, 16, 0, 3600, 1.0)
            .unwrap();
        assert!(
            (episode.hours_ago - 1.0).abs() < 0.01,
            "hours_ago should be ~1.0, got {}",
            episode.hours_ago
        );

        // Event bei Tick 7200, aktuell Tick 7200 (= gerade passiert)
        let (_, episode) = producer
            .event_to_episode(&payload, 17, 7200, 7200, 1.0)
            .unwrap();
        assert!(
            episode.hours_ago.abs() < 0.001,
            "hours_ago should be ~0.0, got {}",
            episode.hours_ago
        );
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
        let producer = EpisodeProducer::new(hippocampus, &[], &es);

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
        let mut producer = EpisodeProducer::new(hippocampus, &[], &es);

        // Vor Registrierung: Agent unbekannt
        let payload = DomainEventPayload::BioActionPerformed {
            agent_id: AgentId(5),
            action: "eat".to_string(),
        };
        assert!(producer
            .event_to_episode(&payload, 18, 0, 10, 1.0)
            .is_none());

        // Nach Registrierung: Agent bekannt
        producer.register_agent(5, "Kevin".to_string());
        let result = producer.event_to_episode(&payload, 18, 0, 10, 1.0);
        assert!(result.is_some());
        assert_eq!(result.unwrap().0, "Kevin");
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
        let mut producer = EpisodeProducer::new(hippocampus, &agents, &event_store);

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
        let mut producer = EpisodeProducer::new(hippocampus, &agents, &event_store);

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
        assert_eq!(
            producer
                .hippocampus()
                .store()
                .load_episode_projection_frontier("Thomas")
                .unwrap()
                .unwrap()
                .last_source_row_id,
            0
        );
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
            let mut producer = EpisodeProducer::new(hippocampus, &agents, &event_store);
            assert_eq!(producer.tick(&event_store, 20, 1.0), 1);
        }
        event_store
            .force_reset_offset(OFFSET_NAME, source_row_id + 100)
            .unwrap();

        let hippocampus = HippocampusService::open(hippocampus_path.to_str().unwrap()).unwrap();
        let mut restarted = EpisodeProducer::new(hippocampus, &agents, &event_store);
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
        let id = stable_episode_id("Thomas", &event.event_id, 1, &digest);
        assert_eq!(id, stable_episode_id("Thomas", &event.event_id, 1, &digest));
        assert_ne!(id, stable_episode_id("Lisa", &event.event_id, 1, &digest));

        let mut changed = event.clone();
        changed.tick += 1;
        assert_ne!(digest, source_request_digest(&changed));
    }
}
