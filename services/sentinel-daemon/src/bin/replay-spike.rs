//! #490 replay-spike — EXPERIMENTAL Go/No-Go harness for bounded same-machine exact replay (TM-3).
//!
//! NOT production code. This drives genuine re-execution of the ECS tick loop with tick-pinned
//! scripted inputs (not event-apply: `BioStateUpdated`/`RoomPhysicsUpdated` events are sampled, so
//! event-apply alone cannot reproduce a byte-identical intermediate state). It builds its own world
//! via `create_simulation_world` + `spawn_agent`, feeds inputs through the same channel resources the
//! daemon uses, and compares canonicalized ECS state hashes (STRICT + CORE). No daemon, no redb,
//! gateway/judge off -> 0 tokens. See `docs/spikes/SPIKE-490-exact-replay.md`.
//!
//! Test ladder: T1 engine determinism, T2 restore-vs-live (AC-2), T3 two replays (AC-1),
//! T4 event-log-as-input, T5 per-tick trace, T6 order-probe, T7 PSI negative control.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{sync_channel, Receiver, Sender, SyncSender};
use std::sync::Mutex;
use std::time::Instant;

use anyhow::{Context, Result};
use bevy_ecs::prelude::{Schedule, World};
use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

use sentinel_common::events::DomainEvent;
use sentinel_common::feature_flags::RuntimeFlags;
use sentinel_common::room::BuildingConfig;
use sentinel_common::{
    ActionType, AgentAction, AgentId, EcsSnapshot, EventType, OperatorChaosCommand,
    OperatorCommand, Perception, Tick, Timestamp,
};
use sentinel_ecs::world::ROOM_IDS;
use sentinel_ecs::{
    create_simulation_world, restore_ecs_state, snapshot_ecs_state, spawn_agent, ActionReceiver,
    ActiveAgentsThisTick, LimboEventStore, OperatorCommandReceiver, PerceptionSender, PsiMetrics,
    RoomDistanceMap, RoomInfoMap, SimulationTime,
};
use sentinel_limbo::EventStore;

const AGENT_COUNT: u16 = 26;
const SIM_HOUR_START: f32 = 8.0;
/// Large enough that one tick's perception sends never fill the channel before the post-tick drain.
const PERCEPTION_CHANNEL_CAP: usize = 100_000;

// ───────────────────────── State hash (AC-1/AC-2/AC-3) ─────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct StateHashes {
    /// Full canonical snapshot.
    strict: String,
    /// Without `PerceptionState`/`EventQueue` — separates perception-text gaps from sim-core divergence.
    core: String,
}

/// Re-serialize JSON bytes through `serde_json::Value` so HashMap key order is canonical (BTreeMap).
fn canonical_json(bytes: &[u8]) -> Vec<u8> {
    match serde_json::from_slice::<serde_json::Value>(bytes) {
        Ok(v) => serde_json::to_vec(&v).unwrap_or_else(|_| bytes.to_vec()),
        Err(_) => bytes.to_vec(),
    }
}

fn bincode_bytes<T: Serialize>(value: &T) -> Vec<u8> {
    bincode::serde::encode_to_vec(value, bincode::config::legacy())
        .expect("bincode legacy encode of canonical snapshot")
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    format!("{:x}", h.finalize())
}

/// Canonicalize an `EcsSnapshot` for hashing.
/// N1: sort every component vec by agent id (Bevy allocation order is not stable across restore);
/// N2: `transit_correlation_id := None` (UUIDv4 event identity, not sim state);
/// N3: canonicalize the chaos/stimuli JSON (HashMap byte order);
/// N4: nothing else (f32 stays a bit pattern via the legacy bincode codec).
fn canonicalize(mut s: EcsSnapshot) -> EcsSnapshot {
    s.positions.sort_by_key(|(id, _)| *id);
    s.bio_states.sort_by_key(|(id, _)| *id);
    s.personalities.sort_by_key(|(id, _)| *id);
    s.moods.sort_by_key(|(id, _)| *id);
    s.perception_states.sort_by_key(|(id, _)| *id);
    s.work_contexts.sort_by_key(|(id, _)| *id);
    s.agent_capabilities.sort_by_key(|(id, _)| *id);
    s.event_queues.sort_by_key(|(id, _)| *id);
    s.identities.sort_by_key(|(id, _)| *id);
    s.shift_infos.sort_by_key(|(id, _)| *id);
    s.relationships.sort_by_key(|(id, _)| *id);
    s.llm_configs.sort_by_key(|(id, _)| *id);
    // Task entities have no agent u16 key — order by their canonical bincode encoding.
    s.task_states.sort_by_cached_key(bincode_bytes);
    // N2: drop the per-action UUID that leaks into ECS state.
    for (_, p) in s.positions.iter_mut() {
        p.transit_correlation_id = None;
    }
    // N3: HashMap-order-independent JSON.
    s.active_chaos_json = canonical_json(&s.active_chaos_json);
    s.active_stimuli_json = canonical_json(&s.active_stimuli_json);
    s
}

fn state_hashes(world: &mut World) -> StateHashes {
    let canon = canonicalize(snapshot_ecs_state(world));
    let strict = sha256_hex(&bincode_bytes(&canon));
    let mut core = canon;
    core.perception_states.clear();
    core.event_queues.clear();
    let core = sha256_hex(&bincode_bytes(&core));
    StateHashes { strict, core }
}

// ───────────────────────── Scenario / scripted inputs ─────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Variant {
    /// TTLs/cooldowns do not span the anchor -> expected PASS.
    Clean,
    /// Chat/smell/cooldown laid across the anchor -> deliberately provokes the restore gaps.
    GapProbe,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
enum InputKind {
    Action(AgentAction),
    Chaos(OperatorChaosCommand),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ScriptedInput {
    tick: u64,
    kind: InputKind,
}

fn room(i: usize) -> String {
    ROOM_IDS[i % ROOM_IDS.len()].to_string()
}

fn action(
    tick: u64,
    agent: u16,
    action_type: ActionType,
    target_room: Option<String>,
    content: Option<&str>,
) -> ScriptedInput {
    ScriptedInput {
        tick,
        kind: InputKind::Action(AgentAction {
            agent_id: AgentId(agent),
            action_type,
            target_room,
            target_agent: None,
            content: content.map(String::from),
            timestamp: Timestamp(tick),
            tick: Tick(tick),
        }),
    }
}

/// Build the scripted input list deterministically from the window size + variant.
/// Anchor is at 40% of the window.
fn build_script(window: u64, variant: Variant) -> Vec<ScriptedInput> {
    let anchor = anchor_tick(window);
    let mut s = Vec::new();
    for tick in 1..=window {
        if tick % 25 == 0 {
            let a = ((tick / 25) % AGENT_COUNT as u64) as u16 + 1;
            s.push(action(
                tick,
                a,
                ActionType::Move,
                Some(room((tick / 25) as usize)),
                None,
            ));
        }
        if tick % 30 == 0 {
            let a = ((tick / 30) % AGENT_COUNT as u64) as u16 + 1;
            s.push(action(
                tick,
                a,
                ActionType::Chat,
                None,
                Some("status update"),
            ));
        }
        if tick % 47 == 0 {
            s.push(action(
                tick,
                3,
                ActionType::ToolUse,
                None,
                Some("drink_coffee"),
            ));
        }
        if tick % 53 == 0 {
            s.push(action(tick, 7, ActionType::ToolUse, None, Some("eat_meal")));
        }
        if tick % 61 == 0 {
            s.push(action(
                tick,
                11,
                ActionType::ToolUse,
                None,
                Some("use_bathroom"),
            ));
        }
    }
    // One operator chaos at 15% of the window, with script-fixed ids (deterministic).
    let chaos_tick = (window as f64 * 0.15).floor().max(1.0) as u64;
    s.push(ScriptedInput {
        tick: chaos_tick,
        kind: InputKind::Chaos(OperatorChaosCommand {
            event_id: "spike-chaos-0001".into(),
            correlation_id: "spike-chaos-corr-0001".into(),
            operation_id: "spike-chaos-op-0001".into(),
            room_id: "meetingraum-01".into(),
            chaos_type: EventType::FireAlarmDrill,
            description: "scripted drill".into(),
            duration_ticks: Some(120),
        }),
    });
    if variant == Variant::GapProbe {
        // Lay smell (coffee) + chat just before the anchor so their TTLs span it -> restore gaps.
        s.push(action(
            anchor.saturating_sub(2),
            5,
            ActionType::ToolUse,
            None,
            Some("drink_coffee"),
        ));
        s.push(action(
            anchor.saturating_sub(1),
            9,
            ActionType::Chat,
            None,
            Some("pre-anchor chatter"),
        ));
        s.push(action(
            anchor.saturating_sub(1),
            13,
            ActionType::Move,
            Some(room(5)),
            None,
        ));
    }
    s.sort_by_key(|i| i.tick);
    s
}

fn anchor_tick(window: u64) -> u64 {
    (window as f64 * 0.40).floor().max(1.0) as u64
}

/// PSI input per tick. Variant A = constant 0; Variant B = a deterministic trace that crosses the
/// bio thresholds (cpu/mem). `delta` offsets the trace (used by T7 to force a different trace).
fn psi_value(tick: u64, window: u64, scripted: bool, delta: f64) -> (f64, f64) {
    if !scripted {
        return (0.0, 0.0);
    }
    let lo = (window as f64 * 0.30) as u64;
    let hi = (window as f64 * 0.33) as u64;
    let mlo = (window as f64 * 0.50) as u64;
    let mhi = (window as f64 * 0.52) as u64;
    let cpu = if tick >= lo && tick <= hi {
        60.0 + delta
    } else {
        0.0
    };
    let mem = if tick >= mlo && tick <= mhi {
        80.0 + delta
    } else {
        0.0
    };
    (cpu, mem)
}

// ───────────────────────── Simulation harness ─────────────────────────

struct Sim {
    world: World,
    schedule: Schedule,
    action_tx: Sender<AgentAction>,
    operator_tx: Sender<OperatorCommand>,
    _perception_tx_keepalive: SyncSender<Perception>,
    perception_rx: Receiver<Perception>,
    sim_hour: f32,
}

fn rooms_path(explicit: &Option<PathBuf>) -> PathBuf {
    if let Some(p) = explicit {
        return p.clone();
    }
    if let Ok(root) = std::env::var("SENTINEL_REPO_ROOT") {
        return Path::new(&root).join("config/rooms.toml");
    }
    if let Ok(cwd) = std::env::current_dir() {
        let c = cwd.join("config/rooms.toml");
        if c.exists() {
            return c;
        }
    }
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config/rooms.toml")
}

impl Sim {
    /// Fresh world with 26 agents and the daemon's channel resources. `limbo` optionally persists
    /// emitted events (needed only for T4). No redb store (irrelevant to the state question).
    fn new(rooms: &Path, limbo: Option<EventStore>) -> Result<Self> {
        let (mut world, schedule) = create_simulation_world();
        let cfg = BuildingConfig::load(rooms).context("load rooms.toml")?;
        world.insert_resource(RoomDistanceMap::from_building_config(&cfg));
        world.insert_resource(RoomInfoMap::from_building_config(&cfg));
        world.insert_resource(PsiMetrics::default());

        for id in 1..=AGENT_COUNT {
            let idx = (id as usize).saturating_sub(1) % ROOM_IDS.len();
            spawn_agent(
                &mut world,
                AgentId(id),
                &format!("Agent-{id:02}"),
                "Benchmark",
                1,
                ROOM_IDS[idx],
            );
        }

        let (action_tx, action_rx) = std::sync::mpsc::channel::<AgentAction>();
        let (operator_tx, operator_rx) = std::sync::mpsc::channel::<OperatorCommand>();
        let (perception_tx, perception_rx) = sync_channel::<Perception>(PERCEPTION_CHANNEL_CAP);
        world.insert_resource(ActionReceiver(Mutex::new(action_rx)));
        world.insert_resource(OperatorCommandReceiver(Mutex::new(operator_rx)));
        world.insert_resource(PerceptionSender(perception_tx.clone()));
        if let Some(store) = limbo {
            world.insert_resource(LimboEventStore(std::sync::Arc::new(store)));
        }

        Ok(Self {
            world,
            schedule,
            action_tx,
            operator_tx,
            _perception_tx_keepalive: perception_tx,
            perception_rx,
            sim_hour: SIM_HOUR_START,
        })
    }

    fn seed_sim_hour(&mut self, sim_hour: f32) {
        self.sim_hour = sim_hour;
    }

    /// Run ticks `(from, to]`, feeding scripted inputs and PSI. Returns the per-tick STRICT hash
    /// trace when `trace` is set (for T5).
    fn run(
        &mut self,
        script: &[ScriptedInput],
        from: u64,
        to: u64,
        psi: &dyn Fn(u64) -> (f64, f64),
        mut trace: Option<&mut Vec<(u64, String)>>,
    ) {
        for tick in (from + 1)..=to {
            for input in script.iter().filter(|i| i.tick == tick) {
                match &input.kind {
                    InputKind::Action(a) => {
                        let _ = self.action_tx.send(a.clone());
                    }
                    InputKind::Chaos(c) => {
                        let _ = self.operator_tx.send(OperatorCommand::Chaos(c.clone()));
                    }
                }
            }
            {
                let mut time = self.world.resource_mut::<SimulationTime>();
                time.tick = Tick(tick);
                time.tick_count = tick;
                time.delta_seconds = 1.0;
                self.sim_hour = (self.sim_hour + 1.0 / 3600.0) % 24.0;
                time.sim_hour = self.sim_hour;
            }
            {
                let (cpu, mem) = psi(tick);
                let mut m = self.world.resource_mut::<PsiMetrics>();
                m.cpu_avg10 = cpu;
                m.mem_avg10 = mem;
            }
            self.schedule.run(&mut self.world);
            // Drain the perception channel each tick (operator_command_system uses a blocking send).
            while self.perception_rx.try_recv().is_ok() {}
            if let Some(mut active) = self.world.get_resource_mut::<ActiveAgentsThisTick>() {
                active.0.clear();
            }
            if let Some(t) = trace.as_deref_mut() {
                let h = state_hashes(&mut self.world).strict;
                t.push((tick, h));
            }
        }
    }

    fn anchor(&mut self) -> EcsSnapshot {
        snapshot_ecs_state(&mut self.world)
    }

    fn restore(&mut self, anchor: &EcsSnapshot) {
        restore_ecs_state(&mut self.world, anchor);
        self.seed_sim_hour(anchor.sim_hour);
    }

    fn hashes(&mut self) -> StateHashes {
        state_hashes(&mut self.world)
    }
}

// ───────────────────────── Test ladder ─────────────────────────

fn live_to(
    rooms: &Path,
    window: u64,
    variant: Variant,
    scripted_psi: bool,
) -> Result<(EcsSnapshot, StateHashes)> {
    // Returns the anchor snapshot (at 40%) and the final state hash at the target tick.
    let script = build_script(window, variant);
    let anchor_t = anchor_tick(window);
    let mut sim = Sim::new(rooms, None)?;
    let psi = |t: u64| psi_value(t, window, scripted_psi, 0.0);
    sim.run(&script, 0, anchor_t, &psi, None);
    let anchor = sim.anchor();
    sim.run(&script, anchor_t, window, &psi, None);
    Ok((anchor, sim.hashes()))
}

fn replay_from(
    rooms: &Path,
    anchor: &EcsSnapshot,
    window: u64,
    variant: Variant,
    scripted_psi: bool,
    psi_delta: f64,
) -> Result<StateHashes> {
    let script = build_script(window, variant);
    let anchor_t = anchor.sim_tick;
    let mut sim = Sim::new(rooms, None)?;
    sim.restore(anchor);
    let psi = |t: u64| psi_value(t, window, scripted_psi, psi_delta);
    sim.run(&script, anchor_t, window, &psi, None);
    Ok(sim.hashes())
}

#[derive(Serialize)]
struct RunReport {
    window: u64,
    variant: String,
    psi: String,
    anchor_tick: u64,
    t1_engine_determinism: bool,
    t1_hash_a: String,
    t1_hash_b: String,
    t2_restore_vs_live: bool,
    t2_live: StateHashes,
    t2_replay: StateHashes,
    t3_two_replays: bool,
    t7_negative_control_sensitive: bool,
    core_matches: bool,
}

fn run_all(rooms: &Path, window: u64, variant: Variant, scripted_psi: bool) -> Result<RunReport> {
    let script = build_script(window, variant);
    let psi = |t: u64| psi_value(t, window, scripted_psi, 0.0);

    // T1: two full live runs from tick 0 -> equal final hash (engine determinism).
    let mut s1 = Sim::new(rooms, None)?;
    s1.run(&script, 0, window, &psi, None);
    let h1 = s1.hashes();
    let mut s2 = Sim::new(rooms, None)?;
    s2.run(&script, 0, window, &psi, None);
    let h2 = s2.hashes();
    let t1 = h1 == h2;

    // T2: live@target vs restore(anchor)+replay(anchor,target].
    let (anchor, live_hash) = live_to(rooms, window, variant, scripted_psi)?;
    let replay_hash = replay_from(rooms, &anchor, window, variant, scripted_psi, 0.0)?;
    let t2 = live_hash == replay_hash;

    // T3: two replays of the same range -> equal.
    let replay_hash_2 = replay_from(rooms, &anchor, window, variant, scripted_psi, 0.0)?;
    let t3 = replay_hash == replay_hash_2;

    // T7 negative control: a replay with the PSI input REMOVED (zero) must produce a DIFFERENT hash
    // than the scripted-PSI replay — proving the hash is sensitive to a real state input (anti-cheat).
    // The mem-PSI window sits after the 40% anchor, so it is always inside the replay range and
    // crosses the bio stress threshold. (Comparing 80 vs 85 would not: both cross the same threshold.)
    let t7 = if scripted_psi {
        let psi_off = replay_from(rooms, &anchor, window, variant, false, 0.0)?;
        psi_off != replay_hash
    } else {
        true // constant-0 PSI run: sensitivity is verified by the scripted-PSI run instead
    };

    Ok(RunReport {
        window,
        variant: format!("{variant:?}"),
        psi: if scripted_psi {
            "scripted".into()
        } else {
            "zero".into()
        },
        anchor_tick: anchor.sim_tick,
        t1_engine_determinism: t1,
        t1_hash_a: h1.strict.clone(),
        t1_hash_b: h2.strict.clone(),
        t2_restore_vs_live: t2,
        t2_live: live_hash.clone(),
        t2_replay: replay_hash.clone(),
        t3_two_replays: t3,
        t7_negative_control_sensitive: t7,
        core_matches: live_hash.core == replay_hash.core,
    })
}

/// T6: identical anchor, 1 tick, repeated -> hash must be constant (Bevy/HashMap/task order).
fn order_probe(
    rooms: &Path,
    window: u64,
    variant: Variant,
    scripted_psi: bool,
    repeat: u32,
) -> Result<(bool, String)> {
    let script = build_script(window, variant);
    let anchor_t = anchor_tick(window);
    let mut base = Sim::new(rooms, None)?;
    let psi = |t: u64| psi_value(t, window, scripted_psi, 0.0);
    base.run(&script, 0, anchor_t, &psi, None);
    let anchor = base.anchor();

    let mut first: Option<String> = None;
    let mut stable = true;
    for _ in 0..repeat {
        let mut s = Sim::new(rooms, None)?;
        s.restore(&anchor);
        s.run(&script, anchor_t, anchor_t + 1, &psi, None);
        let h = s.hashes().strict;
        match &first {
            None => first = Some(h),
            Some(f) => {
                if *f != h {
                    stable = false;
                    break;
                }
            }
        }
    }
    Ok((stable, first.unwrap_or_default()))
}

/// T5: per-tick STRICT-hash trace of live vs replay -> first divergence tick (or None).
fn trace_divergence(
    rooms: &Path,
    window: u64,
    variant: Variant,
    scripted_psi: bool,
) -> Result<(Option<u64>, usize)> {
    let script = build_script(window, variant);
    let anchor_t = anchor_tick(window);
    let psi = |t: u64| psi_value(t, window, scripted_psi, 0.0);

    let mut live = Sim::new(rooms, None)?;
    live.run(&script, 0, anchor_t, &psi, None);
    let anchor = live.anchor();
    let mut live_trace = Vec::new();
    live.run(&script, anchor_t, window, &psi, Some(&mut live_trace));

    let mut rep = Sim::new(rooms, None)?;
    rep.restore(&anchor);
    let mut rep_trace = Vec::new();
    rep.run(&script, anchor_t, window, &psi, Some(&mut rep_trace));

    let first = live_trace
        .iter()
        .zip(rep_trace.iter())
        .find(|((_, lh), (_, rh))| lh != rh)
        .map(|((t, _), _)| *t);
    Ok((first, live_trace.len()))
}

/// Reconstruct agent-action inputs from persisted `agent_action_received` events in `(from, to]`.
/// Internally-derived actions (`content == "autonomy:bio_emergency"`) are excluded (they are
/// re-derived during replay). Operator chaos is NOT reconstructable from the agent-action log —
/// that gap is the TM-3 finding (the event log lacks `duration_ticks`/source fields).
fn reconstruct_inputs(events: &[DomainEvent], from: u64, to: u64) -> Vec<ScriptedInput> {
    let mut out = Vec::new();
    for e in events {
        if e.event_type != "agent_action_received" || e.tick <= from || e.tick > to {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&e.payload) else {
            continue;
        };
        let content = v.get("content").and_then(|c| c.as_str()).map(String::from);
        if content.as_deref() == Some("autonomy:bio_emergency") {
            continue;
        }
        let action_type = match v.get("action_type").and_then(|a| a.as_str()).unwrap_or("") {
            "Move" => ActionType::Move,
            "Chat" => ActionType::Chat,
            "ToolUse" => ActionType::ToolUse,
            "Emote" => ActionType::Emote,
            "PhoneCall" => ActionType::PhoneCall,
            _ => continue,
        };
        let agent_id = v.get("agent_id").and_then(|a| a.as_u64()).unwrap_or(0) as u16;
        let target_room = v
            .get("target_room")
            .and_then(|r| r.as_str())
            .map(String::from);
        out.push(ScriptedInput {
            tick: e.tick,
            kind: InputKind::Action(AgentAction {
                agent_id: AgentId(agent_id),
                action_type,
                target_room,
                target_agent: None,
                content,
                timestamp: Timestamp(e.tick),
                tick: Tick(e.tick),
            }),
        });
    }
    out.sort_by_key(|i| i.tick);
    out
}

/// T4: is the persisted event log a sufficient input log? Replays from log-reconstructed inputs and
/// compares against the script-based replay. Returns (script_replay, event_replay, reconstructed_count,
/// script_action_count). A mismatch traced solely to non-reconstructable operator chaos is a finding.
fn replay_from_events(
    rooms: &Path,
    window: u64,
    variant: Variant,
    scripted_psi: bool,
) -> Result<(StateHashes, StateHashes, usize, usize)> {
    let script = build_script(window, variant);
    let anchor_t = anchor_tick(window);
    let psi = |t: u64| psi_value(t, window, scripted_psi, 0.0);

    let tmp = std::env::temp_dir().join(format!("replay-spike-{}-{window}.db", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let store = EventStore::open(tmp.to_str().context("tmp db path")?)?;

    let mut live = Sim::new(rooms, Some(store))?;
    live.run(&script, 0, anchor_t, &psi, None);
    let anchor = live.anchor();
    live.run(&script, anchor_t, window, &psi, None);

    let reader = EventStore::open_readonly(tmp.to_str().context("tmp db path")?)?;
    let events = reader.get_events_since(0, 10_000_000)?;
    let reconstructed = reconstruct_inputs(&events, anchor_t, window);

    let script_replay = replay_from(rooms, &anchor, window, variant, scripted_psi, 0.0)?;

    let mut ev = Sim::new(rooms, None)?;
    ev.restore(&anchor);
    ev.run(&reconstructed, anchor_t, window, &psi, None);
    let event_replay = ev.hashes();

    let script_actions = script
        .iter()
        .filter(|i| i.tick > anchor_t && i.tick <= window && matches!(i.kind, InputKind::Action(_)))
        .count();

    let _ = std::fs::remove_file(&tmp);
    Ok((
        script_replay,
        event_replay,
        reconstructed.len(),
        script_actions,
    ))
}

// ───────────────────────── CLI ─────────────────────────

#[derive(Parser)]
#[command(
    name = "replay-spike",
    about = "#490 experimental exact-replay determinism spike (TM-3 Go/No-Go)"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// In-process ladder (T1/T2/T3/T7) for one window/variant/psi -> JSON report.
    RunAll {
        #[arg(long, default_value_t = 1000)]
        window: u64,
        #[arg(long, default_value = "clean")]
        variant: String,
        #[arg(long, default_value = "scripted")]
        psi: String,
        #[arg(long)]
        rooms: Option<PathBuf>,
    },
    /// T6 order-probe: identical anchor, 1 tick, repeated.
    OrderProbe {
        #[arg(long, default_value_t = 1000)]
        window: u64,
        #[arg(long, default_value = "clean")]
        variant: String,
        #[arg(long, default_value = "scripted")]
        psi: String,
        #[arg(long, default_value_t = 100)]
        repeat: u32,
        #[arg(long)]
        rooms: Option<PathBuf>,
    },
    /// Cross-process: run live to the target, write the anchor + final hash to files.
    Live {
        #[arg(long, default_value_t = 1000)]
        window: u64,
        #[arg(long, default_value = "clean")]
        variant: String,
        #[arg(long, default_value = "scripted")]
        psi: String,
        #[arg(long)]
        anchor_out: PathBuf,
        #[arg(long)]
        hash_out: PathBuf,
        #[arg(long)]
        rooms: Option<PathBuf>,
    },
    /// Cross-process: restore the anchor and replay to the target, write the final hash.
    Replay {
        #[arg(long, default_value_t = 1000)]
        window: u64,
        #[arg(long, default_value = "clean")]
        variant: String,
        #[arg(long, default_value = "scripted")]
        psi: String,
        #[arg(long)]
        anchor: PathBuf,
        #[arg(long)]
        hash_out: PathBuf,
        #[arg(long)]
        rooms: Option<PathBuf>,
    },
    /// Compare two hash files (equal -> exit 0, differ -> exit 1).
    Compare {
        #[arg(long)]
        a: PathBuf,
        #[arg(long)]
        b: PathBuf,
    },
    /// T5: per-tick hash trace of live vs replay -> first divergence tick.
    Trace {
        #[arg(long, default_value_t = 1000)]
        window: u64,
        #[arg(long, default_value = "clean")]
        variant: String,
        #[arg(long, default_value = "scripted")]
        psi: String,
        #[arg(long)]
        rooms: Option<PathBuf>,
    },
    /// T4: replay from log-reconstructed inputs vs script replay (event-log-as-input-log question).
    EventReplay {
        #[arg(long, default_value_t = 1000)]
        window: u64,
        #[arg(long, default_value = "clean")]
        variant: String,
        #[arg(long, default_value = "scripted")]
        psi: String,
        #[arg(long)]
        rooms: Option<PathBuf>,
    },
    /// Benchmark: live-compute vs replay wall-clock for a window (Instant, no criterion).
    Bench {
        #[arg(long, default_value_t = 1000)]
        window: u64,
        #[arg(long, default_value = "clean")]
        variant: String,
        #[arg(long)]
        rooms: Option<PathBuf>,
    },
}

fn parse_variant(s: &str) -> Variant {
    match s {
        "gap-probe" | "gap_probe" => Variant::GapProbe,
        _ => Variant::Clean,
    }
}

fn scripted_psi(s: &str) -> bool {
    s != "zero"
}

fn write_bincode<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    std::fs::write(path, bincode_bytes(value)).with_context(|| format!("write {}", path.display()))
}

fn read_anchor(path: &Path) -> Result<EcsSnapshot> {
    let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let (snap, _) =
        bincode::serde::decode_from_slice::<EcsSnapshot, _>(&bytes, bincode::config::legacy())
            .context("decode anchor")?;
    Ok(snap)
}

fn main() -> Result<()> {
    // Make the chaos gate explicit + deterministic in the report (reads SENTINEL_CHAOS_ENABLED once).
    let flags = RuntimeFlags::init();

    let cli = Cli::parse();
    match cli.command {
        Command::RunAll {
            window,
            variant,
            psi,
            rooms,
        } => {
            let r = rooms_path(&rooms);
            let report = run_all(&r, window, parse_variant(&variant), scripted_psi(&psi))?;
            let mut v = serde_json::to_value(&report)?;
            v["chaos_enabled"] = json!(flags.chaos_enabled);
            println!("{}", serde_json::to_string_pretty(&v)?);
        }
        Command::OrderProbe {
            window,
            variant,
            psi,
            repeat,
            rooms,
        } => {
            let r = rooms_path(&rooms);
            let (stable, hash) = order_probe(
                &r,
                window,
                parse_variant(&variant),
                scripted_psi(&psi),
                repeat,
            )?;
            println!(
                "{}",
                json!({"t6_order_stable": stable, "repeat": repeat, "hash": hash})
            );
        }
        Command::Live {
            window,
            variant,
            psi,
            anchor_out,
            hash_out,
            rooms,
        } => {
            let r = rooms_path(&rooms);
            let (anchor, hash) = live_to(&r, window, parse_variant(&variant), scripted_psi(&psi))?;
            write_bincode(&anchor_out, &anchor)?;
            std::fs::write(&hash_out, serde_json::to_vec(&hash)?)?;
            println!(
                "{}",
                json!({"live_hash": hash, "anchor_tick": anchor.sim_tick})
            );
        }
        Command::Replay {
            window,
            variant,
            psi,
            anchor,
            hash_out,
            rooms,
        } => {
            let r = rooms_path(&rooms);
            let snap = read_anchor(&anchor)?;
            let hash = replay_from(
                &r,
                &snap,
                window,
                parse_variant(&variant),
                scripted_psi(&psi),
                0.0,
            )?;
            std::fs::write(&hash_out, serde_json::to_vec(&hash)?)?;
            println!("{}", json!({"replay_hash": hash}));
        }
        Command::Compare { a, b } => {
            let ha: StateHashes = serde_json::from_slice(&std::fs::read(&a)?)?;
            let hb: StateHashes = serde_json::from_slice(&std::fs::read(&b)?)?;
            let equal = ha == hb;
            println!("{}", json!({"equal": equal, "a": ha, "b": hb}));
            if !equal {
                std::process::exit(1);
            }
        }
        Command::Trace {
            window,
            variant,
            psi,
            rooms,
        } => {
            let r = rooms_path(&rooms);
            let (first, ticks) =
                trace_divergence(&r, window, parse_variant(&variant), scripted_psi(&psi))?;
            println!(
                "{}",
                json!({"first_divergence_tick": first, "traced_ticks": ticks})
            );
        }
        Command::EventReplay {
            window,
            variant,
            psi,
            rooms,
        } => {
            let r = rooms_path(&rooms);
            let (script_replay, event_replay, reconstructed, script_actions) =
                replay_from_events(&r, window, parse_variant(&variant), scripted_psi(&psi))?;
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "script_replay": script_replay,
                    "event_replay": event_replay,
                    "match": script_replay == event_replay,
                    "reconstructed_inputs": reconstructed,
                    "script_actions_in_window": script_actions,
                    "note": "operator chaos is not reconstructable from the agent-action log (TM-3 field gap)"
                }))?
            );
        }
        Command::Bench {
            window,
            variant,
            rooms,
        } => {
            let r = rooms_path(&rooms);
            let var = parse_variant(&variant);
            let script = build_script(window, var);
            let psi = |t: u64| psi_value(t, window, true, 0.0);
            // Live compute time.
            let t0 = Instant::now();
            let mut sim = Sim::new(&r, None)?;
            sim.run(&script, 0, window, &psi, None);
            let live_ms = t0.elapsed().as_secs_f64() * 1000.0;
            // Anchor + replay time.
            let (anchor, _) = live_to(&r, window, var, true)?;
            let t1 = Instant::now();
            let _ = replay_from(&r, &anchor, window, var, true, 0.0)?;
            let replay_ms = t1.elapsed().as_secs_f64() * 1000.0;
            let snap_bytes = bincode_bytes(&anchor).len();
            println!(
                "{}",
                json!({
                    "window": window,
                    "live_compute_ms": live_ms,
                    "replay_ms": replay_ms,
                    "anchor_tick": anchor.sim_tick,
                    "snapshot_bytes": snap_bytes
                })
            );
        }
    }
    Ok(())
}

// ───────────────────────── Unit tests (hash normalization) ─────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_json_sorts_object_keys() {
        let a = br#"{"b":1,"a":2}"#;
        let b = br#"{"a":2,"b":1}"#;
        assert_eq!(
            canonical_json(a),
            canonical_json(b),
            "key order canonicalized"
        );
    }

    #[test]
    fn canonical_json_passthrough_on_invalid() {
        let raw = b"not json";
        assert_eq!(canonical_json(raw), raw.to_vec());
    }

    #[test]
    fn build_script_is_deterministic() {
        let a = build_script(1000, Variant::Clean);
        let b = build_script(1000, Variant::Clean);
        assert_eq!(bincode_bytes(&a), bincode_bytes(&b));
        assert!(a.iter().all(|i| i.tick >= 1));
        // ticks are sorted
        assert!(a.windows(2).all(|w| w[0].tick <= w[1].tick));
    }

    #[test]
    fn gap_probe_adds_pre_anchor_residue() {
        let anchor = anchor_tick(1000);
        let clean = build_script(1000, Variant::Clean).len();
        let gap = build_script(1000, Variant::GapProbe).len();
        assert!(gap > clean, "gap-probe injects extra pre-anchor inputs");
        let gp = build_script(1000, Variant::GapProbe);
        assert!(gp.iter().any(|i| i.tick == anchor - 1));
    }

    #[test]
    fn sha256_hex_is_stable() {
        assert_eq!(sha256_hex(b"abc"), sha256_hex(b"abc"));
        assert_ne!(sha256_hex(b"abc"), sha256_hex(b"xyz"));
    }

    fn action_event(
        tick: u64,
        agent: u16,
        action_type: &str,
        content: Option<&str>,
    ) -> DomainEvent {
        let payload = serde_json::json!({
            "agent_id": agent,
            "action_type": action_type,
            "target_room": serde_json::Value::Null,
            "content": content,
        })
        .to_string();
        DomainEvent::new("agent_action_received", "AGENT-01", &payload, "corr", tick)
    }

    #[test]
    fn reconstruct_inputs_maps_agent_actions_in_range() {
        let events = vec![
            action_event(45, 1, "Move", None), // in (40, 100]
            action_event(40, 2, "Chat", None), // == anchor -> excluded (not > from)
            action_event(50, 3, "ToolUse", Some("autonomy:bio_emergency")), // internally derived -> excluded
            action_event(60, 4, "Chat", Some("hello")),                     // in range
            DomainEvent::new("chaos_triggered", "room", "{}", "c", 55), // not an agent action -> excluded
            action_event(150, 5, "Move", None),                         // out of range
        ];
        let out = reconstruct_inputs(&events, 40, 100);
        assert_eq!(out.len(), 2, "only the two valid in-range agent actions");
        assert_eq!(out[0].tick, 45);
        assert_eq!(out[1].tick, 60);
        match &out[0].kind {
            InputKind::Action(a) => {
                assert_eq!(a.agent_id, AgentId(1));
                assert_eq!(a.action_type, ActionType::Move);
            }
            _ => panic!("expected action"),
        }
    }
}
