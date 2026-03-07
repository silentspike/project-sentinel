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
    Emotion, Perception, Tick,
};
use sentinel_limbo::EventStore;
use sentinel_redb::StateStore;
use std::collections::HashMap;
use std::sync::Arc;

/// Aktive Gerueche pro Raum (ephemere ECS Resource).
///
/// Wird von input_system/autonomy_system/smell_system befuellt und von
/// perception_system gelesen. Cleanup abgelaufener Smells bei jedem Tick.
#[derive(Resource, Default, Debug, Clone)]
pub struct ActiveSmells {
    /// Key: room_id, Value: Liste aktiver Gerueche
    pub smells: HashMap<String, Vec<ActiveSmell>>,
}

/// Ein einzelner aktiver Geruch in einem Raum.
#[derive(Debug, Clone)]
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

/// Simulationszeit-Resource (muss vor jedem Schedule::run() aktualisiert werden)
#[derive(Resource, Debug, Clone)]
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
}

impl RoomDistanceMap {
    /// Erstellt die Distance-Map aus einer BuildingConfig (BFS fuer alle Paare).
    pub fn from_building_config(config: &sentinel_common::room::BuildingConfig) -> Self {
        let mut distances = std::collections::HashMap::new();
        for room in &config.rooms {
            for other in &config.rooms {
                if let Some(dist) = config.shortest_distance(&room.id, &other.id) {
                    distances.insert((room.id.clone(), other.id.clone()), dist);
                }
            }
        }
        Self { distances }
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
    // aktuelle WorkContext-Werte (has_deadline, in_meeting) sieht.
    schedule.add_systems(input_system.in_set(SimulationPhase::Input));
    schedule.add_systems(
        work_context_system
            .in_set(SimulationPhase::Input)
            .after(input_system),
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
                room_id: "empfang".to_string(),
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
        room_id: "empfang".to_string(),
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
