//! Episode Producer — Konvertiert DomainEvents aus Limbo zu Hippocampus-Episoden.
//!
//! Laeuft periodisch im ECS Tick-Loop (alle N Ticks), liest neue Events
//! aus dem Limbo EventStore via Cursor und erzeugt Episode-Objekte fuer
//! den HippocampusService. Nightrun konsolidiert diese spaeter.

use std::collections::HashMap;

use sentinel_common::events::DomainEventPayload;
use sentinel_hippocampus::{Episode, HippocampusService};
use sentinel_limbo::EventStore;
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
    /// Monoton steigender Episode-ID-Zaehler.
    next_episode_id: u64,
    /// Zaehler fuer aufeinanderfolgende Laeufe ohne konvertierbare Events (Starvation-Diagnostik).
    empty_runs: u32,
}

/// Offset-Name fuer die Limbo-Offset-Tabelle (Cursor-Persistierung).
const OFFSET_NAME: &str = "episode_producer";

impl EpisodeProducer {
    /// Erstellt einen neuen EpisodeProducer.
    ///
    /// Laedt den Cursor aus dem persistierten Offset. Beim allerersten Start
    /// wird der Cursor auf die aktuelle max Event-ID gesetzt (skip history).
    pub fn new(
        hippocampus: HippocampusService,
        agents: &[(u16, String)],
        event_store: &EventStore,
    ) -> Self {
        let agent_names: HashMap<u16, String> = agents.iter().cloned().collect();

        // Cursor laden: gespeicherter Offset → oder max rowid (skip history)
        let last_event_id = match event_store.get_offset(OFFSET_NAME) {
            Ok(Some(offset)) => {
                info!(offset, "Episode Producer: Cursor aus Offset geladen");
                offset
            }
            _ => {
                let max_id = event_store.max_event_rowid().unwrap_or(0);
                info!(max_id, "Episode Producer: Erster Start, skip history");
                max_id
            }
        };

        Self {
            hippocampus,
            last_event_id,
            agent_names,
            next_episode_id: 1,
            empty_runs: 0,
        }
    }

    /// Gibt eine Referenz auf den HippocampusService zurueck.
    pub fn hippocampus(&self) -> &HippocampusService {
        &self.hippocampus
    }

    /// Registriert einen neuen Agenten (z.B. bei Schichtwechsel).
    pub fn register_agent(&mut self, id: u16, name: String) {
        self.agent_names.insert(id, name);
    }

    /// Ob dieser Tick ein Produktionslauf sein soll.
    pub fn should_run(&self, tick: u64) -> bool {
        tick > 0 && tick.is_multiple_of(PRODUCE_INTERVAL_TICKS)
    }

    /// Verarbeitet neue Events aus Limbo und erzeugt Episoden.
    ///
    /// Gibt die Anzahl produzierter Episoden zurueck.
    pub fn tick(&mut self, event_store: &EventStore, current_tick: u64, tick_rate_s: f64) -> usize {
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

        // Cursor aktualisieren + persistieren
        if let Some((last_id, _)) = events.last() {
            self.last_event_id = *last_id;
            if let Err(e) = event_store.update_offset(OFFSET_NAME, *last_id) {
                warn!(error = %e, "Episode Producer: Offset speichern fehlgeschlagen");
            }
        }

        // Events → Episoden konvertieren, gruppiert nach Agent
        let mut episodes_by_agent: HashMap<String, Vec<Episode>> = HashMap::new();

        for (_, event) in &events {
            let payload: DomainEventPayload = match serde_json::from_str(&event.payload) {
                Ok(p) => p,
                Err(_) => continue,
            };

            if let Some((agent_name, episode)) =
                self.event_to_episode(&payload, event.tick, current_tick, tick_rate_s)
            {
                episodes_by_agent
                    .entry(agent_name)
                    .or_default()
                    .push(episode);
            }
        }

        // Episoden pro Agent persistieren
        let mut total = 0;
        for (agent, episodes) in &episodes_by_agent {
            let count = episodes.len();
            // Erste Episode pro Agent loggen (Diagnose)
            if let Some(ep) = episodes.first() {
                debug!(
                    agent = %agent,
                    id = ep.id,
                    summary = %ep.summary,
                    relevance = ep.relevance,
                    emotion = ep.emotion,
                    hours_ago = ep.hours_ago,
                    tags = ?ep.tags,
                    "Episode sample"
                );
            }
            if let Err(e) = self.hippocampus.record_episodes(agent, episodes) {
                warn!(agent = %agent, error = %e, "Episoden speichern fehlgeschlagen");
            } else {
                total += count;
            }
        }

        if total > 0 {
            self.empty_runs = 0;
            info!(
                episodes = total,
                agents = episodes_by_agent.len(),
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

    /// Konvertiert einen DomainEventPayload in eine Episode (wenn relevant).
    fn event_to_episode(
        &mut self,
        payload: &DomainEventPayload,
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

                let id = self.next_id();
                Some((
                    name.clone(),
                    Episode {
                        id,
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
                let id = self.next_id();
                Some((
                    name.clone(),
                    Episode {
                        id,
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
                let id = self.next_id();
                let summary = format!("Chaos: {event_type:?} - {description}");
                Some((
                    "_building".to_string(),
                    Episode {
                        id,
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

    fn next_id(&mut self) -> u64 {
        let id = self.next_episode_id;
        self.next_episode_id += 1;
        id
    }
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

    #[test]
    fn test_agent_action_produces_episode() {
        let (hippocampus, dir) = temp_hippocampus();
        let es = temp_event_store(&dir);
        let agents = vec![(1, "Thomas".to_string()), (2, "Lisa".to_string())];
        let mut producer = EpisodeProducer::new(hippocampus, &agents, &es);

        let payload = DomainEventPayload::AgentActionReceived {
            agent_id: AgentId(1),
            action_type: "talk".to_string(),
            content: Some("Wir haben ein Problem mit dem Deadline".to_string()),
            target_room: Some("meetingraum-01".to_string()),
            source: None,
        };

        let result = producer.event_to_episode(&payload, 100, 200, 1.0);
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
        let mut producer = EpisodeProducer::new(hippocampus, &agents, &es);

        let payload = DomainEventPayload::BioActionPerformed {
            agent_id: AgentId(1),
            action: "eat_meal".to_string(),
        };

        let result = producer.event_to_episode(&payload, 50, 100, 1.0);
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
        let mut producer = EpisodeProducer::new(hippocampus, &[], &es);

        let payload = DomainEventPayload::ChaosTriggered {
            event_type: sentinel_common::EventType::PrinterBroken,
            target_room: Some("buero-dev-1".to_string()),
            description: "Drucker streikt wieder".to_string(),
            duration_ticks: 0,
        };

        let result = producer.event_to_episode(&payload, 0, 100, 1.0);
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
        let mut producer = EpisodeProducer::new(hippocampus, &[], &es);

        let payload = DomainEventPayload::AgentActionReceived {
            agent_id: AgentId(99),
            action_type: "talk".to_string(),
            content: None,
            target_room: None,
            source: None,
        };

        let result = producer.event_to_episode(&payload, 0, 100, 1.0);
        assert!(result.is_none(), "Unknown agent should return None");
    }

    #[test]
    fn test_transit_event_ignored() {
        let (hippocampus, dir) = temp_hippocampus();
        let es = temp_event_store(&dir);
        let mut producer = EpisodeProducer::new(hippocampus, &[], &es);

        let payload = DomainEventPayload::TransitCompleted {
            agent_id: AgentId(1),
            room_id: "kueche".to_string(),
        };

        let result = producer.event_to_episode(&payload, 0, 100, 1.0);
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
    fn test_episode_id_increments() {
        let (hippocampus, dir) = temp_hippocampus();
        let es = temp_event_store(&dir);
        let agents = vec![(1, "Thomas".to_string())];
        let mut producer = EpisodeProducer::new(hippocampus, &agents, &es);

        let payload = DomainEventPayload::BioActionPerformed {
            agent_id: AgentId(1),
            action: "drink".to_string(),
        };

        let (_, ep1) = producer.event_to_episode(&payload, 0, 10, 1.0).unwrap();
        let (_, ep2) = producer.event_to_episode(&payload, 5, 10, 1.0).unwrap();
        assert_eq!(ep1.id, 1);
        assert_eq!(ep2.id, 2);
    }

    #[test]
    fn test_hours_ago_calculation() {
        let (hippocampus, dir) = temp_hippocampus();
        let es = temp_event_store(&dir);
        let agents = vec![(1, "Thomas".to_string())];
        let mut producer = EpisodeProducer::new(hippocampus, &agents, &es);

        let payload = DomainEventPayload::BioActionPerformed {
            agent_id: AgentId(1),
            action: "eat_meal".to_string(),
        };

        // Event bei Tick 0, aktuell Tick 3600 (= 1 Stunde bei 1s Tick-Rate)
        let (_, episode) = producer.event_to_episode(&payload, 0, 3600, 1.0).unwrap();
        assert!(
            (episode.hours_ago - 1.0).abs() < 0.01,
            "hours_ago should be ~1.0, got {}",
            episode.hours_ago
        );

        // Event bei Tick 7200, aktuell Tick 7200 (= gerade passiert)
        let (_, episode) = producer
            .event_to_episode(&payload, 7200, 7200, 1.0)
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
        assert!(producer.event_to_episode(&payload, 0, 10, 1.0).is_none());

        // Nach Registrierung: Agent bekannt
        producer.register_agent(5, "Kevin".to_string());
        let result = producer.event_to_episode(&payload, 0, 10, 1.0);
        assert!(result.is_some());
        assert_eq!(result.unwrap().0, "Kevin");
    }
}
