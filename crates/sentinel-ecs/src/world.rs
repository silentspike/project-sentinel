//! ECS World Setup und Agent Spawning.
//!
//! Erstellt die ECS World mit allen Systems in korrekter Reihenfolge
//! und bietet die Funktion zum Spawnen von Agenten.

use super::autonomy::AutonomyCooldown;
use super::components::*;
use super::systems::*;
use bevy_ecs::prelude::*;
use sentinel_common::{
    agent_config::PersonalityConfig, AgentAction, AgentId, DomainEvent, DomainEventPayload,
    Emotion, EventType, OperatorCommand, Perception, RoomStimulusType, Tick,
};
use sentinel_limbo::EventStore;
use sentinel_redb::StateStore;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

/// Aktive Gerueche pro Raum (ephemere ECS Resource).
///
/// Wird von input_system/autonomy_system/smell_system befuellt und von
/// perception_system gelesen. Cleanup abgelaufener Smells bei jedem Tick.
#[derive(Resource, Default, Debug, Clone, Serialize, Deserialize)]
pub struct ActiveSmells {
    /// Key: room_id, Value: Liste aktiver Gerueche
    pub smells: HashMap<String, Vec<ActiveSmell>>,
}

/// Ein einzelner aktiver Geruch in einem Raum.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActiveSmell {
    pub smell_type: String,
    pub intensity: f32,
    pub created_tick: u64,
    pub duration_ticks: u64,
}

impl ActiveSmells {
    /// Fuegt einen neuen Geruch in einen Raum ein.
    pub fn add(
        &mut self,
        room_id: &str,
        smell_type: String,
        intensity: f32,
        created_tick: u64,
        duration_ticks: u64,
    ) {
        self.smells
            .entry(room_id.to_string())
            .or_default()
            .push(ActiveSmell {
                smell_type,
                intensity,
                created_tick,
                duration_ticks,
            });
    }

    /// Gibt alle noch aktiven Gerueche fuer einen Raum zurueck.
    pub fn get_active(&self, room_id: &str, current_tick: u64) -> Vec<&ActiveSmell> {
        self.smells
            .get(room_id)
            .map(|smells| {
                smells
                    .iter()
                    .filter(|s| current_tick < s.created_tick + s.duration_ticks)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Entfernt abgelaufene Gerueche aus allen Raeumen.
    pub fn cleanup(&mut self, current_tick: u64) {
        self.smells.retain(|_, smells| {
            smells.retain(|s| current_tick < s.created_tick + s.duration_ticks);
            !smells.is_empty()
        });
    }
}

/// Aktive Chaos-Events pro Raum (ephemere ECS Resource).
///
/// Wird vom Zufalls-Chaos und spaeter auch vom Operator-Pfad befuellt.
/// Pro Raum ist zunaechst genau ein aktives Chaos erlaubt.
#[derive(Resource, Default, Debug, Clone, Serialize, Deserialize)]
pub struct ActiveChaos {
    /// Key: room_id, Value: aktives Chaos-Event
    pub events: HashMap<String, ActiveChaosEvent>,
}

/// Ein einzelnes aktives Chaos-Event in einem Raum.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActiveChaosEvent {
    pub event_type: EventType,
    pub description: String,
    pub created_tick: u64,
    pub duration_ticks: u64,
}

impl ActiveChaos {
    /// Setzt oder ersetzt das aktive Chaos fuer einen Raum.
    pub fn set(
        &mut self,
        room_id: &str,
        event_type: EventType,
        description: String,
        created_tick: u64,
        duration_ticks: u64,
    ) {
        self.events.insert(
            room_id.to_string(),
            ActiveChaosEvent {
                event_type,
                description,
                created_tick,
                duration_ticks,
            },
        );
    }

    /// Gibt das noch aktive Chaos fuer einen Raum zurueck.
    pub fn get_active(&self, room_id: &str, current_tick: u64) -> Option<&ActiveChaosEvent> {
        self.events
            .get(room_id)
            .filter(|event| current_tick < event.created_tick.saturating_add(event.duration_ticks))
    }

    /// Gibt alle Raeume mit noch aktivem Chaos zurueck.
    pub fn active_rooms(&self, current_tick: u64) -> Vec<&str> {
        self.events
            .iter()
            .filter_map(|(room_id, event)| {
                (current_tick < event.created_tick.saturating_add(event.duration_ticks))
                    .then_some(room_id.as_str())
            })
            .collect()
    }

    /// Entfernt abgelaufene Chaos-Events aus allen Raeumen.
    pub fn cleanup(&mut self, current_tick: u64) {
        self.events.retain(|_, event| {
            current_tick < event.created_tick.saturating_add(event.duration_ticks)
        });
    }
}

/// Aktive manuelle Raumreize pro Raum und Reiztyp.
#[derive(Resource, Default, Debug, Clone, Serialize, Deserialize)]
pub struct ActiveRoomStimuli {
    /// Key: room_id, Value: aktiver Reiz je Typ
    pub entries: HashMap<String, HashMap<RoomStimulusType, ActiveRoomStimulus>>,
}

/// Ein einzelner aktiver Raumreiz.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActiveRoomStimulus {
    pub stimulus_type: RoomStimulusType,
    pub delta: f32,
    pub description: String,
    pub created_tick: u64,
    pub duration_ticks: u64,
}

impl ActiveRoomStimuli {
    /// Setzt oder ersetzt einen aktiven Raumreiz fuer Raum+Typ.
    pub fn set(
        &mut self,
        room_id: &str,
        stimulus_type: RoomStimulusType,
        delta: f32,
        description: String,
        created_tick: u64,
        duration_ticks: u64,
    ) {
        self.entries.entry(room_id.to_string()).or_default().insert(
            stimulus_type,
            ActiveRoomStimulus {
                stimulus_type,
                delta,
                description,
                created_tick,
                duration_ticks,
            },
        );
    }

    /// Gibt den noch aktiven Reiz fuer Raum+Typ zurueck.
    pub fn get_active(
        &self,
        room_id: &str,
        stimulus_type: RoomStimulusType,
        current_tick: u64,
    ) -> Option<&ActiveRoomStimulus> {
        self.entries
            .get(room_id)
            .and_then(|room| room.get(&stimulus_type))
            .filter(|event| current_tick < event.created_tick.saturating_add(event.duration_ticks))
    }

    /// Summiert die aktuell aktiven Deltas fuer einen Raum und Typ.
    pub fn delta_for(
        &self,
        room_id: &str,
        stimulus_type: RoomStimulusType,
        current_tick: u64,
    ) -> f32 {
        self.get_active(room_id, stimulus_type, current_tick)
            .map(|event| event.delta)
            .unwrap_or(0.0)
    }

    /// Gibt alle Raeume mit mindestens einem aktiven Reiz zurueck.
    pub fn active_rooms(&self, current_tick: u64) -> Vec<&str> {
        self.entries
            .iter()
            .filter_map(|(room_id, stimuli)| {
                stimuli
                    .values()
                    .any(|event| {
                        current_tick < event.created_tick.saturating_add(event.duration_ticks)
                    })
                    .then_some(room_id.as_str())
            })
            .collect()
    }

    /// Entfernt abgelaufene Raumreize.
    pub fn cleanup(&mut self, current_tick: u64) {
        self.entries.retain(|_, stimuli| {
            stimuli.retain(|_, event| {
                current_tick < event.created_tick.saturating_add(event.duration_ticks)
            });
            !stimuli.is_empty()
        });
    }
}

/// Letzter berechneter Physics-Snapshot pro Raum fuer echte Reaktionslogik.
#[derive(Resource, Default, Debug, Clone, Serialize, Deserialize)]
pub struct RoomPhysicsState {
    pub rooms: HashMap<String, RoomPhysicsSnapshot>,
}

/// Physik-Snapshot eines Raums im aktuellen Tick.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomPhysicsSnapshot {
    pub tick: u64,
    pub occupant_count: u32,
    pub temperature: f32,
    pub co2_ppm: f32,
    pub noise_db: f32,
}

impl RoomPhysicsState {
    pub fn set(
        &mut self,
        room_id: &str,
        tick: u64,
        occupant_count: u32,
        temperature: f32,
        co2_ppm: f32,
        noise_db: f32,
    ) {
        self.rooms.insert(
            room_id.to_string(),
            RoomPhysicsSnapshot {
                tick,
                occupant_count,
                temperature,
                co2_ppm,
                noise_db,
            },
        );
    }

    pub fn get(&self, room_id: &str) -> Option<&RoomPhysicsSnapshot> {
        self.rooms.get(room_id)
    }
}

/// Simulationszeit-Resource (muss vor jedem Schedule::run() aktualisiert werden)
#[derive(Resource, Debug, Clone, Serialize, Deserialize)]
pub struct SimulationTime {
    pub tick: Tick,
    pub tick_count: u64,
    pub delta_seconds: f32,
    pub sim_hour: f32, // 0.0-24.0 simulierte Tageszeit
}

impl Default for SimulationTime {
    fn default() -> Self {
        Self {
            tick: Tick(0),
            tick_count: 0,
            delta_seconds: 1.0, // 1 Sekunde pro Tick default
            sim_hour: 8.0,      // Arbeitsbeginn 08:00
        }
    }
}

/// Optional persistence backend for batched ECS snapshots.
#[derive(Resource, Clone)]
pub struct RedbStateStore {
    pub store: Arc<StateStore>,
    pub persist_every_n_ticks: u64,
}

impl RedbStateStore {
    pub fn new(store: StateStore) -> Self {
        Self {
            store: Arc::new(store),
            // Interim default from 2026-02-13 ablation: balanced durability/latency profile.
            persist_every_n_ticks: 20,
        }
    }

    pub fn with_tick_interval(mut self, interval: u64) -> Self {
        self.persist_every_n_ticks = interval.max(1);
        self
    }
}

/// Real-path persistence telemetry (no benchmark proxy layer).
///
/// This resource is updated directly by `persist_system` so benchmark runs can
/// observe flush/batch behavior without altering the data path.
#[derive(Resource, Debug, Clone)]
pub struct PersistTelemetry {
    pub enabled: bool,
    pub interval_ticks: u64,
    pub ticks_observed: u64,
    pub skipped_ticks: u64,
    pub flush_attempts: u64,
    pub flush_success: u64,
    pub flush_failures: u64,
    pub batch_size_last: u64,
    pub batch_size_sum: u64,
    pub batch_size_max: u64,
    pub flush_latency_us_sum: f64,
    pub flush_latency_us_max: f64,
    pub queue_depth_current: u64,
    pub queue_depth_max: u64,
    pub drop_count: u64,
    pub coalesce_count: u64,
    pub write_behind_enabled: bool,
}

impl Default for PersistTelemetry {
    fn default() -> Self {
        Self {
            enabled: false,
            interval_ticks: 0,
            ticks_observed: 0,
            skipped_ticks: 0,
            flush_attempts: 0,
            flush_success: 0,
            flush_failures: 0,
            batch_size_last: 0,
            batch_size_sum: 0,
            batch_size_max: 0,
            flush_latency_us_sum: 0.0,
            flush_latency_us_max: 0.0,
            queue_depth_current: 0,
            queue_depth_max: 0,
            drop_count: 0,
            coalesce_count: 0,
            write_behind_enabled: false,
        }
    }
}

impl PersistTelemetry {
    pub fn avg_flush_latency_us(&self) -> f64 {
        if self.flush_attempts == 0 {
            return 0.0;
        }
        self.flush_latency_us_sum / self.flush_attempts as f64
    }

    pub fn avg_batch_size(&self) -> f64 {
        if self.flush_attempts == 0 {
            return 0.0;
        }
        self.batch_size_sum as f64 / self.flush_attempts as f64
    }
}

/// Empfaengt AgentActions vom externen Zenoh-Subscriber (oder Test-Code).
/// Mutex-wrapped weil std::sync::mpsc::Receiver nicht Sync ist (bevy_ecs braucht Sync).
#[derive(Resource)]
pub struct ActionReceiver(pub std::sync::Mutex<std::sync::mpsc::Receiver<AgentAction>>);

/// Sammelt Agent-IDs die in diesem Tick eine Action ausgefuehrt haben.
///
/// Befuellt von input_system (LLM-Actions) und autonomy_system (Bio-P0-Actions).
/// Orchestrator liest nach schedule.run() und synct zu RuntimeOrchestrator.record_activity().
#[derive(Resource, Default)]
pub struct ActiveAgentsThisTick(pub Vec<AgentId>);

/// In-Memory Buffer fuer gesprochene Nachrichten pro Raum.
///
/// ZERO Disk-Writes — Chat ist bereits als AgentActionReceived Event persistiert.
/// Kaskade-Dampening: Max 1 Chat/Tick/Raum + Max 2 Responses/Agent/120 Ticks.
#[derive(Resource, Default, Debug, Clone)]
pub struct RoomChatBuffer {
    messages: std::collections::HashMap<String, Vec<RoomChatEntry>>,
    /// Letzter Chat-Tick pro Raum (1 Chat/Tick/Raum Rate-Limit).
    last_chat_tick: std::collections::HashMap<String, u64>,
    /// Letzter Tick an dem ein Agent Chat gehoert hat (Cooldown).
    chat_cooldowns: std::collections::HashMap<String, u64>,
    /// Chat-Response-Counter pro Agent (window_start_tick, count). Max 2/120 Ticks.
    chat_response_counts: std::collections::HashMap<String, (u64, u32)>,
}

/// Eine einzelne Chat-Nachricht in einem Raum.
#[derive(Debug, Clone)]
pub struct RoomChatEntry {
    pub agent_name: String,
    pub content: String,
    pub tick: u64,
    pub ttl_ticks: u64,
    pub addressed_agents: Vec<String>,
}

const CHAT_TTL_TICKS: u64 = 120;
const MAX_CHAT_RESPONSES_PER_WINDOW: u32 = 2;
const CHAT_RESPONSE_WINDOW_TICKS: u64 = 120;
const MAX_CONTENT_LEN: usize = 500;

impl RoomChatBuffer {
    /// Fuegt eine Chat-Nachricht hinzu. Gibt false zurueck bei Rate-Limit (1 Chat/Tick/Raum).
    pub fn add(
        &mut self,
        room_id: &str,
        agent_name: String,
        content: String,
        tick: u64,
        all_agent_names: &[String],
    ) -> bool {
        // Rate-Limit: Max 1 Chat pro Tick pro Raum
        if let Some(&last_tick) = self.last_chat_tick.get(room_id) {
            if last_tick == tick {
                return false;
            }
        }
        self.last_chat_tick.insert(room_id.to_string(), tick);

        // Content trimmen
        let trimmed = if content.len() > MAX_CONTENT_LEN {
            format!("{}...", &content[..MAX_CONTENT_LEN - 3])
        } else {
            content
        };

        // Direkt-Ansprache erkennen (Vorname oder voller Name im Text)
        let addressed: Vec<String> = all_agent_names
            .iter()
            .filter(|name| {
                let first = name.split_whitespace().next().unwrap_or(name);
                (trimmed.contains(first) || trimmed.contains(name.as_str())) && *name != &agent_name
            })
            .cloned()
            .collect();

        self.messages
            .entry(room_id.to_string())
            .or_default()
            .push(RoomChatEntry {
                agent_name,
                content: trimmed,
                tick,
                ttl_ticks: CHAT_TTL_TICKS,
                addressed_agents: addressed,
            });

        true
    }

    /// Gibt aktuelle Messages fuer einen Raum zurueck (exkludiert eigene + bereits gehoerte).
    pub fn get_recent(
        &self,
        room_id: &str,
        current_tick: u64,
        exclude_agent: &str,
    ) -> Vec<&RoomChatEntry> {
        let cooldown_tick = self.chat_cooldowns.get(exclude_agent).copied().unwrap_or(0);
        self.messages
            .get(room_id)
            .map(|msgs| {
                msgs.iter()
                    .filter(|m| {
                        current_tick < m.tick + m.ttl_ticks
                            && m.agent_name != exclude_agent
                            && m.tick > cooldown_tick
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Markiert dass ein Agent Chat gehoert hat (Cooldown-Update).
    pub fn set_heard(&mut self, agent_name: &str, tick: u64) {
        self.chat_cooldowns.insert(agent_name.to_string(), tick);
    }

    /// Prueft ob ein Agent noch auf Chat reagieren darf (max 2/120 Ticks).
    /// Prueft ob ein Agent noch auf Chat reagieren darf (max 2/120 Ticks).
    /// Inkrementiert den Counter NICHT — dafuer record_response() aufrufen.
    pub fn can_respond(&mut self, agent_name: &str, tick: u64) -> bool {
        let entry = self
            .chat_response_counts
            .entry(agent_name.to_string())
            .or_insert((tick, 0));

        // Window abgelaufen → reset
        if tick.saturating_sub(entry.0) >= CHAT_RESPONSE_WINDOW_TICKS {
            *entry = (tick, 0);
        }

        entry.1 < MAX_CHAT_RESPONSES_PER_WINDOW
    }

    /// Zaehlt eine Chat-Response fuer das Kaskade-Dampening.
    /// NUR aufrufen wenn Agent tatsaechlich etwas gehoert hat.
    pub fn record_response(&mut self, agent_name: &str, tick: u64) {
        let entry = self
            .chat_response_counts
            .entry(agent_name.to_string())
            .or_insert((tick, 0));
        if tick.saturating_sub(entry.0) >= CHAT_RESPONSE_WINDOW_TICKS {
            *entry = (tick, 0);
        }
        entry.1 += 1;
    }

    /// Entfernt abgelaufene Messages.
    pub fn cleanup(&mut self, current_tick: u64) {
        self.messages.retain(|_, msgs| {
            msgs.retain(|m| current_tick < m.tick + m.ttl_ticks);
            !msgs.is_empty()
        });
    }
}

/// Empfaengt Operator-Kommandos fuer manuelles Chaos aus dem Daemon.
#[derive(Resource)]
pub struct OperatorCommandReceiver(
    pub std::sync::Mutex<std::sync::mpsc::Receiver<OperatorCommand>>,
);

/// Sendet Perceptions an den externen Zenoh-Publisher (oder Test-Code).
#[derive(Resource)]
pub struct PerceptionSender(pub std::sync::mpsc::SyncSender<Perception>);

/// Sammelt DomainEvents waehrend eines Ticks. persist_system flusht am Ende.
#[derive(Resource, Default)]
pub struct EventBuffer {
    pub events: Vec<DomainEvent>,
}

/// Wrapper um den Limbo EventStore fuer ECS Resource-Injection.
#[derive(Resource, Clone)]
pub struct LimboEventStore(pub Arc<EventStore>);

/// ECS Resource: Wraps sentinel-wasm ToolRuntime fuer Tool-Dispatch im input_system.
#[derive(Resource)]
pub struct ToolRuntimeResource(pub sentinel_wasm::ToolRuntime);

/// Vorberechnete Raum-Distanzen fuer Transit-Dauer und Smell-Propagation.
///
/// Wird einmal beim Start aus rooms.toml geladen.
/// Key: (from_room, to_room), Value: hop_count
#[derive(Resource, Default, Clone)]
pub struct RoomDistanceMap {
    distances: std::collections::HashMap<(String, String), u32>,
    room_ids: Vec<String>,
}

impl RoomDistanceMap {
    /// Erstellt die Distance-Map aus einer BuildingConfig (BFS fuer alle Paare).
    pub fn from_building_config(config: &sentinel_common::room::BuildingConfig) -> Self {
        let mut distances = std::collections::HashMap::new();
        let room_ids = config.rooms.iter().map(|room| room.id.clone()).collect();
        for room in &config.rooms {
            for other in &config.rooms {
                if let Some(dist) = config.shortest_distance(&room.id, &other.id) {
                    distances.insert((room.id.clone(), other.id.clone()), dist);
                }
            }
        }
        Self {
            distances,
            room_ids,
        }
    }

    /// Gibt die Distanz zwischen zwei Raeumen zurueck (0 = selber Raum).
    pub fn distance(&self, from: &str, to: &str) -> u32 {
        self.distances
            .get(&(from.to_string(), to.to_string()))
            .copied()
            .unwrap_or(2) // Fallback: 2 Hops (mittlere Distanz)
    }

    /// Gibt alle Raeume zurueck die max `max_hops` entfernt sind.
    pub fn rooms_within(&self, from: &str, max_hops: u32) -> Vec<(&str, u32)> {
        self.distances
            .iter()
            .filter(|((f, _), &d)| f == from && d > 0 && d <= max_hops)
            .map(|((_, t), &d)| (t.as_str(), d))
            .collect()
    }

    /// Gibt die bekannte Liste aller Raum-IDs aus `rooms.toml` zurueck.
    pub fn all_rooms(&self) -> &[String] {
        &self.room_ids
    }

    /// Prueft ob ein Raum in der Distance-Map existiert (d.h. in rooms.toml definiert ist).
    pub fn contains(&self, room_id: &str) -> bool {
        self.room_ids.iter().any(|r| r == room_id)
    }
}

/// Zenoh Fan-Out Bridge: Events nach Limbo-Write an async Fanout-Task senden.
///
/// `try_send()` ist non-blocking und sicher aus dem sync ECS-Thread.
/// Wenn der Channel voll ist, werden Events gedroppt (Limbo ist SSOT).
#[derive(Resource, Clone)]
pub struct ZenohFanoutSender {
    pub sender: tokio::sync::mpsc::Sender<DomainEvent>,
}

/// PSI-Metriken als ECS-Resource fuer Bio-Engine Integration.
///
/// Wird vom Daemon mit aktuellen cgroup-PSI-Werten befuellt.
/// Default: 0.0 (kein Druck = kein Bio-Effekt).
#[derive(Resource, Debug, Clone, Default)]
pub struct PsiMetrics {
    pub cpu_avg10: f64,
    pub mem_avg10: f64,
}

/// Attach a redb persistence backend to the simulation world.
pub fn attach_redb_store(world: &mut World, store: StateStore) {
    world.insert_resource(RedbStateStore::new(store));
    if let Some(mut telemetry) = world.get_resource_mut::<PersistTelemetry>() {
        telemetry.enabled = true;
    }
}

/// Erstellt einen neuen ECS World mit allen Systems in korrekter Reihenfolge
pub fn create_simulation_world() -> (World, Schedule) {
    let mut world = World::new();
    let mut schedule = Schedule::default();

    // Resources einfuegen
    world.insert_resource(SimulationTime::default());
    world.insert_resource(PersistTelemetry::default());
    world.insert_resource(EventBuffer::default());
    world.insert_resource(ActiveSmells::default());
    world.insert_resource(ActiveChaos::default());
    world.insert_resource(ActiveRoomStimuli::default());
    world.insert_resource(RoomPhysicsState::default());
    world.insert_resource(ActiveAgentsThisTick::default());
    world.insert_resource(RoomChatBuffer::default());

    // System-Reihenfolge via configure_sets (10 Phasen)
    schedule.configure_sets(
        (
            SimulationPhase::Input,
            SimulationPhase::Biology,
            SimulationPhase::Physics,
            SimulationPhase::Transit,
            SimulationPhase::Chaos,
            SimulationPhase::Mood,
            SimulationPhase::Perception,
            SimulationPhase::Decision,
            SimulationPhase::Output,
            SimulationPhase::Persist,
        )
            .chain(),
    );

    // Systems in ihre Sets einsortieren
    // work_context_system in Input-Phase (VOR Biology), damit bio_system
    // aktuelle WorkContext-Werte (has_deadline, in_meeting, conflict flags) sieht.
    schedule.add_systems(input_system.in_set(SimulationPhase::Input));
    schedule.add_systems(
        operator_command_system
            .in_set(SimulationPhase::Input)
            .after(input_system),
    );
    schedule.add_systems(
        work_context_system
            .in_set(SimulationPhase::Input)
            .after(operator_command_system),
    );
    schedule.add_systems(bio_system.in_set(SimulationPhase::Biology));
    schedule.add_systems(physics_system.in_set(SimulationPhase::Physics));
    schedule.add_systems(transit_system.in_set(SimulationPhase::Transit));
    schedule.add_systems(
        encounter_system
            .in_set(SimulationPhase::Transit)
            .after(transit_system),
    );
    schedule.add_systems(chaos_system.in_set(SimulationPhase::Chaos));
    schedule.add_systems(
        smell_system
            .in_set(SimulationPhase::Chaos)
            .after(chaos_system),
    );
    schedule.add_systems(mood_system.in_set(SimulationPhase::Mood));
    schedule.add_systems(perception_system.in_set(SimulationPhase::Perception));
    schedule.add_systems(super::decision::decision_system.in_set(SimulationPhase::Decision));
    schedule.add_systems(
        super::autonomy::autonomy_system
            .in_set(SimulationPhase::Decision)
            .after(super::decision::decision_system),
    );
    schedule.add_systems(output_system.in_set(SimulationPhase::Output));
    schedule.add_systems(persist_system.in_set(SimulationPhase::Persist));

    (world, schedule)
}

/// Spawnt einen Agenten mit allen 11 Components und Default-Werten.
///
/// Erzeugt ein `AgentSpawned` DomainEvent im EventBuffer (wenn vorhanden),
/// damit Dashboard/Projection Worker ueber neue Agents informiert werden.
pub fn spawn_agent(
    world: &mut World,
    agent_id: AgentId,
    name: &str,
    role: &str,
    shift_set: u8,
    room_id: &str,
) -> Entity {
    let (shift_start, shift_end) = match shift_set {
        1 => (6, 14),  // Fruehschicht
        2 => (14, 22), // Mittelschicht
        3 => (22, 6),  // Spaetschicht
        0 => (0, 0),   // Sonder (24/7)
        _ => (6, 14),  // Fallback: Fruehschicht
    };

    let entity = world
        .spawn((
            AgentIdentity {
                agent_id,
                name: name.to_string(),
                role: role.to_string(),
            },
            Position {
                room_id: room_id.to_string(),
                in_transit: false,
                transit_target: None,
                transit_remaining_ms: 0,
                transit_correlation_id: None,
            },
            BioState {
                hunger: 20.0,
                energy: 80.0,
                caffeine_mg: 0.0,
                bladder: 10.0,
                stress: 15.0,
                social_need: 50.0,
                comfort: 70.0,
            },
            Personality {
                openness: 0.5,
                conscientiousness: 0.5,
                extraversion: 0.5,
                agreeableness: 0.5,
                neuroticism: 0.3,
                caffeine_tolerance: 0.5,
                is_morning_person: true,
            },
            Mood {
                valence: 0.2,
                arousal: 0.3,
                dominant_emotion: Emotion::Neutral,
            },
            PerceptionState {
                environment_text: String::new(),
                body_text: String::new(),
                social_text: String::new(),
                last_updated: Tick(0),
            },
            WorkContext {
                current_task: None,
                in_meeting: false,
                has_deadline: false,
                has_conflict: false,
                conflict_cooldown: 0,
            },
            Relationships {
                affinity: Vec::new(),
            },
            LlmConfig {
                provider: "claude".to_string(),
                model: "claude-sonnet-4-5-20250929".to_string(),
                temperature: 0.7,
                max_tokens: 4096,
            },
            ShiftInfo {
                shift_set,
                shift_start_hour: shift_start,
                shift_end_hour: shift_end,
                is_on_duty: false,
            },
            EventQueue::default(),
            AutonomyCooldown::default(),
            AgentCapabilities::default(),
        ))
        .id();

    // AgentSpawned Event erzeugen (damit Dashboard/Projection Agent kennt)
    let tick = world
        .get_resource::<SimulationTime>()
        .map(|t| t.tick.0)
        .unwrap_or(0);
    let payload = DomainEventPayload::AgentSpawned {
        agent_id,
        name: name.to_string(),
        role: role.to_string(),
        shift_set,
        room_id: room_id.to_string(),
    };
    let event = DomainEvent::new(
        payload.event_type_str(),
        &agent_id.to_string(),
        &payload.to_json(),
        &uuid::Uuid::new_v4().to_string(),
        tick,
    );
    if let Some(mut event_buffer) = world.get_resource_mut::<EventBuffer>() {
        event_buffer.events.push(event);
    }

    entity
}

/// Ueberschreibt die Default-Personality eines gespawnten Agents mit TOML-Werten.
///
/// Muss nach `spawn_agent()` aufgerufen werden. Aendert NUR die ECS Personality-Component,
/// nicht die TOML-Datei (readonly SSOT / "DNA" laut TOGAF-Guide).
pub fn apply_personality(world: &mut World, entity: Entity, cfg: &PersonalityConfig) {
    if let Some(mut p) = world.get_mut::<Personality>(entity) {
        p.openness = cfg.openness;
        p.conscientiousness = cfg.conscientiousness;
        p.extraversion = cfg.extraversion;
        p.agreeableness = cfg.agreeableness;
        p.neuroticism = cfg.neuroticism;
        p.caffeine_tolerance = cfg.caffeine_tolerance;
        p.is_morning_person = cfg.morning_person;
    }
}

/// Ueberschreibt die Default-Capabilities eines gespawnten Agents mit TOML-Werten.
///
/// Muss nach `spawn_agent()` aufgerufen werden.
pub fn apply_capabilities(
    world: &mut World,
    entity: Entity,
    cfg: &sentinel_common::agent_config::CapabilitiesConfig,
) {
    if let Some(mut caps) = world.get_mut::<AgentCapabilities>(entity) {
        caps.tools = cfg.tools.clone();
        caps.sandbox_allowed_paths = cfg.sandbox_allowed_paths.clone();
    }
}

/// Entfernt einen Agenten aus der ECS World anhand seiner AgentId.
/// Gibt `true` zurueck wenn der Agent gefunden und despawned wurde.
pub fn despawn_agent_from_world(world: &mut World, agent_id: AgentId) -> bool {
    let mut query = world.query::<(Entity, &AgentIdentity)>();
    let entity = query
        .iter(world)
        .find(|(_, identity)| identity.agent_id == agent_id)
        .map(|(entity, _)| entity);
    if let Some(entity) = entity {
        world.despawn(entity);
        true
    } else {
        false
    }
}

// ──────────────────────────────────────────────
// Time Machine: ECS Snapshot / Restore
// ──────────────────────────────────────────────

/// Erstellt einen Snapshot des gesamten ECS-Zustands (alle Agent-Entities + Resources).
pub fn snapshot_ecs_state(world: &mut World) -> sentinel_common::EcsSnapshot {
    let mut positions = Vec::new();
    let mut bio_states = Vec::new();
    let mut personalities = Vec::new();
    let mut moods = Vec::new();
    let mut perception_states = Vec::new();
    let mut work_contexts = Vec::new();
    let mut agent_capabilities = Vec::new();
    let mut event_queues = Vec::new();
    let mut identities = Vec::new();
    let mut shift_infos = Vec::new();

    // Alle Agents via AgentIdentity Query (jeder Agent hat diese Component)
    let mut query = world.query::<(
        &AgentIdentity,
        &Position,
        &BioState,
        &Personality,
        &Mood,
        &PerceptionState,
        &WorkContext,
        &AgentCapabilities,
        &EventQueue,
        &ShiftInfo,
    )>();

    for (identity, pos, bio, personality, mood, perception, work, caps, events, shift) in
        query.iter(world)
    {
        let id = identity.agent_id.0;
        identities.push((id, identity.clone()));
        positions.push((id, pos.clone()));
        bio_states.push((id, bio.clone()));
        personalities.push((id, personality.clone()));
        moods.push((id, mood.clone()));
        perception_states.push((id, perception.clone()));
        work_contexts.push((id, work.clone()));
        agent_capabilities.push((id, caps.clone()));
        event_queues.push((id, events.clone()));
        shift_infos.push((id, shift.clone()));
    }

    // Zweite Query fuer Relationships + LlmConfig (bevy tuple limit = 12)
    let mut relationships_vec = Vec::new();
    let mut llm_configs_vec = Vec::new();
    let mut query2 = world.query::<(&AgentIdentity, &Relationships, &LlmConfig)>();
    for (identity, rels, llm) in query2.iter(world) {
        let id = identity.agent_id.0;
        relationships_vec.push((id, rels.clone()));
        llm_configs_vec.push((id, llm.clone()));
    }

    let sim_time = world.get_resource::<SimulationTime>();

    sentinel_common::EcsSnapshot {
        positions,
        bio_states,
        personalities,
        moods,
        perception_states,
        work_contexts,
        agent_capabilities,
        event_queues,
        identities,
        shift_infos,
        relationships: relationships_vec,
        llm_configs: llm_configs_vec,
        sim_tick: sim_time.map(|t| t.tick.0).unwrap_or(0),
        sim_hour: sim_time.map(|t| t.sim_hour).unwrap_or(0.0),
        sim_delta_seconds: sim_time.map(|t| t.delta_seconds).unwrap_or(1.0),
        active_chaos_json: world
            .get_resource::<ActiveChaos>()
            .and_then(|c| serde_json::to_vec(c).ok())
            .unwrap_or_default(),
        active_stimuli_json: world
            .get_resource::<ActiveRoomStimuli>()
            .and_then(|s| serde_json::to_vec(s).ok())
            .unwrap_or_default(),
    }
}

/// Restored den ECS-Zustand aus einem Snapshot.
/// Despawnt ALLE bestehenden Agent-Entities und erstellt neue aus dem Snapshot.
pub fn restore_ecs_state(world: &mut World, snapshot: &sentinel_common::EcsSnapshot) {
    // 1. Alle existierenden Agent-Entities finden und despawnen
    let mut existing: Vec<Entity> = Vec::new();
    {
        let mut query = world.query::<(Entity, &AgentIdentity)>();
        for (entity, _) in query.iter(world) {
            existing.push(entity);
        }
    }
    for entity in existing {
        world.despawn(entity);
    }

    // 2. Agents aus Snapshot respawnen (mit allen Components)
    for (id, identity) in &snapshot.identities {
        let pos = snapshot
            .positions
            .iter()
            .find(|(aid, _)| aid == id)
            .map(|(_, p)| p.clone())
            .unwrap_or(Position {
                room_id: "empfang".to_string(),
                in_transit: false,
                transit_target: None,
                transit_remaining_ms: 0,
                transit_correlation_id: None,
            });
        let bio = snapshot
            .bio_states
            .iter()
            .find(|(aid, _)| aid == id)
            .map(|(_, b)| b.clone())
            .unwrap_or(BioState {
                hunger: 20.0,
                energy: 80.0,
                caffeine_mg: 0.0,
                bladder: 10.0,
                stress: 15.0,
                social_need: 50.0,
                comfort: 70.0,
            });
        let personality = snapshot
            .personalities
            .iter()
            .find(|(aid, _)| aid == id)
            .map(|(_, p)| p.clone())
            .unwrap_or(Personality {
                openness: 0.5,
                conscientiousness: 0.5,
                extraversion: 0.5,
                agreeableness: 0.5,
                neuroticism: 0.3,
                caffeine_tolerance: 0.5,
                is_morning_person: true,
            });
        let mood = snapshot
            .moods
            .iter()
            .find(|(aid, _)| aid == id)
            .map(|(_, m)| m.clone())
            .unwrap_or(Mood {
                valence: 0.2,
                arousal: 0.3,
                dominant_emotion: Emotion::Neutral,
            });
        let perception = snapshot
            .perception_states
            .iter()
            .find(|(aid, _)| aid == id)
            .map(|(_, p)| p.clone())
            .unwrap_or_default();
        let work = snapshot
            .work_contexts
            .iter()
            .find(|(aid, _)| aid == id)
            .map(|(_, w)| w.clone())
            .unwrap_or_default();
        let caps = snapshot
            .agent_capabilities
            .iter()
            .find(|(aid, _)| aid == id)
            .map(|(_, c)| c.clone())
            .unwrap_or_default();
        let events = snapshot
            .event_queues
            .iter()
            .find(|(aid, _)| aid == id)
            .map(|(_, e)| e.clone())
            .unwrap_or_default();
        let shift = snapshot
            .shift_infos
            .iter()
            .find(|(aid, _)| aid == id)
            .map(|(_, s)| s.clone())
            .unwrap_or(ShiftInfo {
                shift_set: 1,
                shift_start_hour: 6,
                shift_end_hour: 14,
                is_on_duty: false,
            });

        let rels = snapshot
            .relationships
            .iter()
            .find(|(aid, _)| aid == id)
            .map(|(_, r)| r.clone())
            .unwrap_or(Relationships {
                affinity: Vec::new(),
            });
        let llm = snapshot
            .llm_configs
            .iter()
            .find(|(aid, _)| aid == id)
            .map(|(_, l)| l.clone())
            .unwrap_or(LlmConfig {
                provider: "claude".to_string(),
                model: "claude-sonnet-4-5-20250929".to_string(),
                temperature: 0.7,
                max_tokens: 4096,
            });

        world.spawn((
            identity.clone(),
            pos,
            bio,
            personality,
            mood,
            perception,
            work,
            rels,
            llm,
            shift,
            events,
            AutonomyCooldown::default(),
            caps,
        ));
    }

    // 3. SimulationTime Resource ueberschreiben
    if let Some(mut sim_time) = world.get_resource_mut::<SimulationTime>() {
        sim_time.tick = Tick(snapshot.sim_tick);
        sim_time.tick_count = snapshot.sim_tick;
        sim_time.sim_hour = snapshot.sim_hour;
        sim_time.delta_seconds = snapshot.sim_delta_seconds;
    }

    // 4. Ephemere Resources restoren (ActiveChaos, ActiveRoomStimuli)
    if !snapshot.active_chaos_json.is_empty() {
        if let Ok(chaos) = serde_json::from_slice::<ActiveChaos>(&snapshot.active_chaos_json) {
            world.insert_resource(chaos);
        }
    }
    if !snapshot.active_stimuli_json.is_empty() {
        if let Ok(stimuli) =
            serde_json::from_slice::<ActiveRoomStimuli>(&snapshot.active_stimuli_json)
        {
            world.insert_resource(stimuli);
        }
    }
}
