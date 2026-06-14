//! #491 (TM-3): Bounded deterministic Replay `(anchor, target]` auf der LIVE-World.
//!
//! Nach `restore_ecs_state(anchor)` fuettert dieses Modul die exakt aufgezeichneten Eingaben
//! (Agent-Aktionen + Operator-Kommandos, rekonstruiert aus dem Event-Log) Tick fuer Tick wieder
//! ein, bis der Ziel-Tick erreicht ist. Der Zustand ist danach byte-identisch zu „damals" (Spike
//! #490: same-machine deterministisch).
//!
//! Seiteneffekt-Gates (sonst wuerde das Replay echte LLM-Calls/Events/NATS ausloesen):
//! - `PerceptionSender`/`ActionReceiver`/`OperatorCommandReceiver` werden auf **Scratch-Channels
//!   GETAUSCHT** (nicht entfernt) — `output_system` mutiert RoomChat/Gaia-Buffer NACH dem
//!   Sender-Guard, ein Removal wuerde diese Mutationen ueberspringen und divergieren. Der echte
//!   LLM-Bridge-Receiver haelt waehrenddessen sein altes Ende und bekommt nichts.
//! - `LimboEventStore`/`ZenohFanoutSender`/`RedbStateStore` werden **ENTFERNT** — `persist_system`
//!   leert den `EventBuffer` bei Absenz (kein Event-Re-Append, kein Zenoh/redb-Write).
//!
//! Setup/Teardown ist explizit (restore auf Ok UND Err). Ein Panic waehrend des Replays ist fatal
//! wie jeder Tick-Loop-Panic — die aktive `RestoreFence` + der Pre-Restore-Safety-Snapshot decken
//! das Recovery beim Neustart ab.

use std::sync::mpsc::{channel, sync_channel, Sender};
use std::sync::Mutex;

use anyhow::Result;
use bevy_ecs::schedule::Schedule;
use bevy_ecs::world::World;
use sentinel_common::{
    ActionType, AgentAction, AgentId, DomainEvent, EventType, OperatorBroadcastCommand,
    OperatorChaosCommand, OperatorCommand, OperatorDmCommand, OperatorGaiaCommand,
    OperatorRoomStimulusCommand, Perception, RoomStimulusType, Tick, Timestamp,
};
use sentinel_ecs::{
    ActionReceiver, ActiveAgentsThisTick, LimboEventStore, OperatorCommandReceiver,
    PerceptionSender, PsiMetrics, RedbStateStore, SimulationTime, ZenohFanoutSender,
    PSI_CPU_STRESS_THRESHOLD, PSI_MEM_STRESS_THRESHOLD,
};

/// Channel-Kapazitaet fuer den Scratch-Perception-Sink: gross genug, dass die Sends eines Ticks den
/// Kanal nie fuellen, bevor der Post-Tick-Drain laeuft (operator_command_system sendet blockierend).
const PERCEPTION_SINK_CAP: usize = 100_000;

/// Eine rekonstruierte Eingabe, die zu ihrem Tick wieder eingespeist wird.
#[derive(Debug, Clone)]
pub enum ReplayInput {
    Action(AgentAction),
    Operator(Box<OperatorCommand>),
}

/// Ergebnis eines Replay-Laufs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReplayReport {
    pub ticks_replayed: u64,
    pub inputs_injected: usize,
    pub psi_band_changes: usize,
}

/// Rekonstruiert die externen Eingaben aus persistierten Events (bereits auf `(anchor, target]`
/// begrenzt durch die Range-Query). Reihenfolge stabil nach `events.id` (= Eingabe-Reihenfolge),
/// daher KEIN Re-Sort: die Range kommt schon `ORDER BY id ASC`.
///
/// Eingespeist werden NUR echte externe Eingaben:
/// - `agent_action_received` mit `source != "autonomy"` (autonome Aktionen erzeugt das
///   deterministische Autonomy-System beim Replay selbst neu -> sonst Doppel-Anwendung),
/// - Operator-Kommandos (`chaos_triggered`, `room_stimulus_applied`, `operator_gaia_sent`,
///   `operator_broadcast_sent`, `operator_dm_sent`).
///
/// Alle anderen Event-Typen sind Outputs (transit/bio/physics/…) und werden NICHT eingespeist.
pub fn reconstruct_inputs(events: &[DomainEvent]) -> Vec<(u64, ReplayInput)> {
    let mut out = Vec::new();
    for e in events {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&e.payload) else {
            continue;
        };
        match e.event_type.as_str() {
            "agent_action_received" => {
                // Autonomy-deriviert: NICHT injizieren. Bevorzugt das neue source-Feld (#491),
                // Fallback auf den alten content-Marker fuer Vor-#491-Events.
                let source = v.get("source").and_then(|s| s.as_str());
                let content = v.get("content").and_then(|c| c.as_str());
                if source == Some("autonomy") || content == Some("autonomy:bio_emergency") {
                    continue;
                }
                let action_type = match v.get("action_type").and_then(|a| a.as_str()).unwrap_or("")
                {
                    "Move" => ActionType::Move,
                    "Chat" => ActionType::Chat,
                    "ToolUse" => ActionType::ToolUse,
                    "Emote" => ActionType::Emote,
                    "PhoneCall" => ActionType::PhoneCall,
                    _ => continue,
                };
                let agent_id = v.get("agent_id").and_then(|a| a.as_u64()).unwrap_or(0) as u16;
                out.push((
                    e.tick,
                    ReplayInput::Action(AgentAction {
                        agent_id: AgentId(agent_id),
                        action_type,
                        target_room: v
                            .get("target_room")
                            .and_then(|r| r.as_str())
                            .map(String::from),
                        target_agent: None,
                        content: content.map(String::from),
                        timestamp: Timestamp(e.tick),
                        tick: Tick(e.tick),
                    }),
                ));
            }
            "chaos_triggered" => {
                let Some(room_id) = v.get("target_room").and_then(|r| r.as_str()) else {
                    continue;
                };
                let Some(chaos_type) = v
                    .get("event_type")
                    .and_then(|t| serde_json::from_value::<EventType>(t.clone()).ok())
                else {
                    continue;
                };
                out.push((
                    e.tick,
                    ReplayInput::Operator(Box::new(OperatorCommand::Chaos(OperatorChaosCommand {
                        event_id: e.event_id.clone(),
                        correlation_id: e.correlation_id.clone(),
                        operation_id: e.operation_id.clone(),
                        room_id: room_id.to_string(),
                        chaos_type,
                        description: v
                            .get("description")
                            .and_then(|d| d.as_str())
                            .unwrap_or("")
                            .to_string(),
                        duration_ticks: v.get("duration_ticks").and_then(|d| d.as_u64()),
                    }))),
                ));
            }
            "room_stimulus_applied" => {
                let Some(room_id) = v.get("room_id").and_then(|r| r.as_str()) else {
                    continue;
                };
                let Some(stimulus_type) = v
                    .get("stimulus_type")
                    .and_then(|t| serde_json::from_value::<RoomStimulusType>(t.clone()).ok())
                else {
                    continue;
                };
                out.push((
                    e.tick,
                    ReplayInput::Operator(Box::new(OperatorCommand::RoomStimulus(
                        OperatorRoomStimulusCommand {
                            event_id: e.event_id.clone(),
                            correlation_id: e.correlation_id.clone(),
                            operation_id: e.operation_id.clone(),
                            room_id: room_id.to_string(),
                            stimulus_type,
                            delta: v.get("delta").and_then(|d| d.as_f64()).unwrap_or(0.0) as f32,
                            duration_ticks: v.get("duration_ticks").and_then(|d| d.as_u64()),
                            description: v
                                .get("description")
                                .and_then(|d| d.as_str())
                                .unwrap_or("")
                                .to_string(),
                        },
                    ))),
                ));
            }
            "operator_gaia_sent" => {
                out.push((
                    e.tick,
                    ReplayInput::Operator(Box::new(OperatorCommand::Gaia(OperatorGaiaCommand {
                        target_agent_id: v
                            .get("target_agent_id")
                            .and_then(|a| a.as_u64())
                            .unwrap_or(0) as u16,
                        thought: v
                            .get("thought")
                            .and_then(|t| t.as_str())
                            .unwrap_or("")
                            .to_string(),
                    }))),
                ));
            }
            "operator_broadcast_sent" => {
                out.push((
                    e.tick,
                    ReplayInput::Operator(Box::new(OperatorCommand::Broadcast(
                        OperatorBroadcastCommand {
                            message: v
                                .get("message")
                                .and_then(|m| m.as_str())
                                .unwrap_or("")
                                .to_string(),
                            broadcast_type: v
                                .get("broadcast_type")
                                .and_then(|t| t.as_str())
                                .unwrap_or("")
                                .to_string(),
                        },
                    ))),
                ));
            }
            "operator_dm_sent" => {
                out.push((
                    e.tick,
                    ReplayInput::Operator(Box::new(OperatorCommand::Dm(OperatorDmCommand {
                        target_agent_id: v
                            .get("target_agent_id")
                            .and_then(|a| a.as_u64())
                            .unwrap_or(0) as u16,
                        message: v
                            .get("message")
                            .and_then(|m| m.as_str())
                            .unwrap_or("")
                            .to_string(),
                        sender_name: v
                            .get("sender_name")
                            .and_then(|s| s.as_str())
                            .unwrap_or("")
                            .to_string(),
                    }))),
                ));
            }
            _ => {}
        }
    }
    out
}

/// PSI-Band-Wechsel aus `psi_band_changed`-Events (`(tick, (cpu_above, mem_above))`), stabil nach id.
pub fn psi_band_schedule(events: &[DomainEvent]) -> Vec<(u64, (bool, bool))> {
    let mut out = Vec::new();
    for e in events {
        if e.event_type != "psi_band_changed" {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&e.payload) else {
            continue;
        };
        let cpu = v
            .get("cpu_above")
            .and_then(|b| b.as_bool())
            .unwrap_or(false);
        let mem = v
            .get("mem_above")
            .and_then(|b| b.as_bool())
            .unwrap_or(false);
        out.push((e.tick, (cpu, mem)));
    }
    out
}

/// PSI-Metriken, die das Band am Tick `tick` reproduzieren (Wert knapp ueber/unter Schwelle —
/// `apply_psi_stress` ist rein schwellenbasiert). `bands` ist nach Tick aufsteigend; gilt das
/// jeweils letzte Band mit `band_tick <= tick`.
fn psi_metrics_at(bands: &[(u64, (bool, bool))], tick: u64) -> (f64, f64) {
    let mut cur = (false, false);
    for (bt, band) in bands {
        if *bt <= tick {
            cur = *band;
        } else {
            break;
        }
    }
    let cpu = if cur.0 {
        PSI_CPU_STRESS_THRESHOLD + 1.0
    } else {
        0.0
    };
    let mem = if cur.1 {
        PSI_MEM_STRESS_THRESHOLD + 1.0
    } else {
        0.0
    };
    (cpu, mem)
}

/// Fuehrt das bounded Replay `(anchor_tick, target_tick]` auf der LIVE-World aus. Die World muss
/// vorher via `restore_ecs_state(anchor)` auf den Anchor-Zustand gesetzt sein; `schedule` ist
/// dasselbe Schedule wie im Tick-Loop. `events` ist der Range `(anchor.last_event_id, target_event_id]`
/// (durch die Range-Query bereits auf `target_event_id` begrenzt -> die Intra-Tick-Grenze am
/// Ziel-Tick ist automatisch eingehalten).
pub fn run_bounded_replay(
    world: &mut World,
    schedule: &mut Schedule,
    events: &[DomainEvent],
    anchor_tick: u64,
    target_tick: u64,
) -> Result<ReplayReport> {
    let inputs = reconstruct_inputs(events);
    let bands = psi_band_schedule(events);

    // ── Setup: 6 Resources gaten ──
    let orig_perception = world.remove_resource::<PerceptionSender>();
    let orig_action = world.remove_resource::<ActionReceiver>();
    let orig_operator = world.remove_resource::<OperatorCommandReceiver>();
    let orig_limbo = world.remove_resource::<LimboEventStore>();
    let orig_zenoh = world.remove_resource::<ZenohFanoutSender>();
    let orig_redb = world.remove_resource::<RedbStateStore>();

    let (action_tx, action_rx) = channel::<AgentAction>();
    let (operator_tx, operator_rx) = channel::<OperatorCommand>();
    let (perception_tx, perception_rx) = sync_channel::<Perception>(PERCEPTION_SINK_CAP);
    world.insert_resource(ActionReceiver(Mutex::new(action_rx)));
    world.insert_resource(OperatorCommandReceiver(Mutex::new(operator_rx)));
    world.insert_resource(PerceptionSender(perception_tx));

    // ── Replay-Schleife (Result, kein early-return -> Teardown laeuft immer) ──
    let result = replay_loop(
        world,
        schedule,
        &inputs,
        &bands,
        anchor_tick,
        target_tick,
        &action_tx,
        &operator_tx,
        &perception_rx,
    );

    // ── Teardown: Scratch raus, Originale zurueck ──
    world.remove_resource::<ActionReceiver>();
    world.remove_resource::<OperatorCommandReceiver>();
    world.remove_resource::<PerceptionSender>();
    if let Some(r) = orig_perception {
        world.insert_resource(r);
    }
    if let Some(r) = orig_action {
        world.insert_resource(r);
    }
    if let Some(r) = orig_operator {
        world.insert_resource(r);
    }
    if let Some(r) = orig_limbo {
        world.insert_resource(r);
    }
    if let Some(r) = orig_zenoh {
        world.insert_resource(r);
    }
    if let Some(r) = orig_redb {
        world.insert_resource(r);
    }

    result
}

#[allow(clippy::too_many_arguments)]
fn replay_loop(
    world: &mut World,
    schedule: &mut Schedule,
    inputs: &[(u64, ReplayInput)],
    bands: &[(u64, (bool, bool))],
    anchor_tick: u64,
    target_tick: u64,
    action_tx: &Sender<AgentAction>,
    operator_tx: &Sender<OperatorCommand>,
    perception_rx: &std::sync::mpsc::Receiver<Perception>,
) -> Result<ReplayReport> {
    let mut report = ReplayReport {
        psi_band_changes: bands.len(),
        ..Default::default()
    };
    // sim_hour aus dem (restaurierten) Anchor-Zustand weiterfuehren.
    let mut sim_hour = world
        .get_resource::<SimulationTime>()
        .map(|t| t.sim_hour)
        .unwrap_or(0.0);
    // #530: den vom Anchor wiederhergestellten delta_seconds REPRODUZIEREN (restore_ecs_state setzt ihn
    // aus EcsSnapshot.sim_delta_seconds, world.rs:1733) statt hardcoded 1.0. Der Live-Tick-Loop nutzt
    // delta = tick_rate * time_scale (orchestrator.rs:4117); bei time_scale != 1.0 wuerde ein
    // 1.0-Schritt sim_hour/sim_delta_seconds (beide gehasht) und die delta-abhaengige Bio-Integration
    // divergieren lassen. Bei time_scale == 1.0 ist anchor_delta == 1.0 -> unveraendert.
    let anchor_delta = world
        .get_resource::<SimulationTime>()
        .map(|t| t.delta_seconds)
        .unwrap_or(1.0);

    for tick in (anchor_tick + 1)..=target_tick {
        // Eingaben dieses Ticks einspeisen.
        for (t, input) in inputs.iter().filter(|(t, _)| *t == tick) {
            debug_assert_eq!(*t, tick);
            match input {
                ReplayInput::Action(a) => {
                    let _ = action_tx.send(a.clone());
                    report.inputs_injected += 1;
                }
                ReplayInput::Operator(c) => {
                    let _ = operator_tx.send((**c).clone());
                    report.inputs_injected += 1;
                }
            }
        }
        // SimulationTime: Inputs sind tick-gepinnt; der Zeit-Schritt wird mit dem Anchor-delta
        // reproduziert (= Live-delta bei konstantem time_scale), nicht hardcoded 1.0 (#530).
        {
            let mut time = world.resource_mut::<SimulationTime>();
            time.tick = Tick(tick);
            time.tick_count = tick;
            time.delta_seconds = anchor_delta;
            sim_hour = (sim_hour + anchor_delta / 3600.0) % 24.0;
            time.sim_hour = sim_hour;
        }
        // PSI-Band setzen (deklarierter Input).
        {
            let (cpu, mem) = psi_metrics_at(bands, tick);
            let mut m = world.resource_mut::<PsiMetrics>();
            m.cpu_avg10 = cpu;
            m.mem_avg10 = mem;
        }
        schedule.run(world);
        // Perception-Senke leeren (operator_command_system sendet blockierend).
        while perception_rx.try_recv().is_ok() {}
        if let Some(mut active) = world.get_resource_mut::<ActiveAgentsThisTick>() {
            active.0.clear();
        }
        report.ticks_replayed += 1;
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sentinel_ecs::hash::state_hashes;
    use sentinel_ecs::{
        create_simulation_world, restore_ecs_state, snapshot_ecs_state, spawn_agent,
    };

    fn world_with_agents(n: u16) -> (World, Schedule) {
        let (mut world, schedule) = create_simulation_world();
        world.insert_resource(PsiMetrics::default());
        for id in 1..=n {
            spawn_agent(
                &mut world,
                AgentId(id),
                &format!("A-{id:02}"),
                "Dev",
                1,
                "empfang",
            );
        }
        (world, schedule)
    }

    fn action_event(tick: u64, agent: u16, action: &str, room: &str) -> DomainEvent {
        let payload = serde_json::json!({
            "type": "AgentActionReceived",
            "agent_id": agent,
            "action_type": action,
            "target_room": room,
            "content": null,
            "source": null,
        });
        DomainEvent::new(
            "agent_action_received",
            &format!("AGENT-{agent:02}"),
            &payload.to_string(),
            "corr-r",
            tick,
        )
    }

    #[test]
    fn reconstruct_skips_autonomy_and_outputs() {
        let mut autonomy = action_event(5, 1, "Move", "kueche");
        autonomy.payload = serde_json::json!({
            "type":"AgentActionReceived","agent_id":1,"action_type":"Move",
            "target_room":"kueche","content":"autonomy:bio_emergency","source":"autonomy"
        })
        .to_string();
        let bio = DomainEvent::new("bio_action_performed", "AGENT-01", "{}", "c", 5);
        let real = action_event(6, 2, "Chat", "empfang");
        let inputs = reconstruct_inputs(&[autonomy, bio, real]);
        assert_eq!(inputs.len(), 1, "nur die echte externe Aktion");
        assert_eq!(inputs[0].0, 6);
    }

    #[test]
    fn psi_metrics_at_picks_last_band() {
        let bands = vec![(10, (true, false)), (20, (true, true))];
        assert_eq!(psi_metrics_at(&bands, 5), (0.0, 0.0));
        assert!(psi_metrics_at(&bands, 15).0 > PSI_CPU_STRESS_THRESHOLD);
        assert_eq!(psi_metrics_at(&bands, 15).1, 0.0);
        assert!(psi_metrics_at(&bands, 25).1 > PSI_MEM_STRESS_THRESHOLD);
    }

    #[test]
    fn live_equals_restore_plus_replay() {
        // AC-1/AC-2-Kern (same-process): live bis target == restore(anchor)+replay(anchor,target].
        let anchor_tick = 20u64;
        let target_tick = 60u64;
        // Skript: ein paar externe Aktionen vor + nach dem Anchor.
        let events: Vec<DomainEvent> = vec![
            action_event(10, 1, "Move", "kueche"),
            action_event(25, 2, "Chat", "empfang"),
            action_event(40, 3, "Move", "flur-eg"),
            action_event(55, 1, "Chat", "kueche"),
        ];

        // (1) Live-Lauf bis target_tick, Anchor-Snapshot bei anchor_tick.
        let (mut w, mut sched) = world_with_agents(4);
        run_ticks(&mut w, &mut sched, &events, 0, anchor_tick);
        let anchor = snapshot_ecs_state(&mut w);
        run_ticks(&mut w, &mut sched, &events, anchor_tick, target_tick);
        let live = state_hashes(&mut w).strict;

        // (2) Frische World -> bis anchor -> restore(anchor) -> run_bounded_replay -> Hash.
        let (mut w2, mut sched2) = world_with_agents(4);
        run_ticks(&mut w2, &mut sched2, &events, 0, anchor_tick);
        restore_ecs_state(&mut w2, &anchor);
        let report = run_bounded_replay(&mut w2, &mut sched2, &events, anchor_tick, target_tick)
            .expect("replay");
        let replayed = state_hashes(&mut w2).strict;

        assert_eq!(report.ticks_replayed, target_tick - anchor_tick);
        assert_eq!(
            live, replayed,
            "live@{target_tick} muss == restore(anchor@{anchor_tick})+replay sein"
        );
    }

    #[test]
    fn replay_reproduces_anchor_delta_at_nonunit_time_scale() {
        // #530: bei time_scale != 1.0 (hier delta_seconds=2.0) muss restore(anchor)+replay den
        // Live-Zustand byte-exakt reproduzieren. Der Replay nutzt den Anchor-delta (aus dem Snapshot,
        // via restore gesetzt), nicht hardcoded 1.0. OHNE den Fix divergieren sim_hour +
        // sim_delta_seconds (beide gehasht) + die delta-abhaengige Bio-Integration -> dieser Test
        // wuerde failen.
        let anchor_tick = 20u64;
        let target_tick = 60u64;
        let events: Vec<DomainEvent> = vec![
            action_event(25, 2, "Chat", "empfang"),
            action_event(40, 3, "Move", "flur-eg"),
        ];

        // Live-Lauf bei delta=2.0 (time_scale=2.0).
        let (mut w, mut sched) = world_with_agents(4);
        w.resource_mut::<SimulationTime>().delta_seconds = 2.0;
        run_ticks(&mut w, &mut sched, &events, 0, anchor_tick);
        let anchor = snapshot_ecs_state(&mut w);
        assert_eq!(
            anchor.sim_delta_seconds, 2.0,
            "Anchor-Snapshot traegt den delta_seconds"
        );
        run_ticks(&mut w, &mut sched, &events, anchor_tick, target_tick);
        let live = state_hashes(&mut w);

        // Restore (setzt delta_seconds=2.0 aus dem Snapshot) + Replay -> muss live reproduzieren.
        let (mut w2, mut sched2) = world_with_agents(4);
        w2.resource_mut::<SimulationTime>().delta_seconds = 2.0;
        run_ticks(&mut w2, &mut sched2, &events, 0, anchor_tick);
        restore_ecs_state(&mut w2, &anchor);
        run_bounded_replay(&mut w2, &mut sched2, &events, anchor_tick, target_tick)
            .expect("replay");
        let replayed = state_hashes(&mut w2);

        assert_eq!(
            live.strict, replayed.strict,
            "STRICT exakt bei delta=2.0 (Anchor-delta reproduziert)"
        );
        assert_eq!(
            live.core, replayed.core,
            "CORE exakt bei delta=2.0 (Anchor-delta reproduziert)"
        );
    }

    // Hilfs-Tick-Loop fuer den Test (spiegelt run_bounded_replay ohne Resource-Gating, da die
    // Test-World ohnehin keine Limbo/Zenoh/Redb-Resources haelt).
    fn run_ticks(
        world: &mut World,
        schedule: &mut Schedule,
        events: &[DomainEvent],
        from: u64,
        to: u64,
    ) {
        let inputs = reconstruct_inputs(events);
        let bands = psi_band_schedule(events);
        // Test-World braucht Scratch-Channels (ohne LLM-Bridge dahinter).
        let (action_tx, action_rx) = channel::<AgentAction>();
        let (operator_tx, operator_rx) = channel::<OperatorCommand>();
        let (perception_tx, perception_rx) = sync_channel::<Perception>(PERCEPTION_SINK_CAP);
        world.insert_resource(ActionReceiver(Mutex::new(action_rx)));
        world.insert_resource(OperatorCommandReceiver(Mutex::new(operator_rx)));
        world.insert_resource(PerceptionSender(perception_tx));
        let mut sim_hour = world
            .get_resource::<SimulationTime>()
            .map(|t| t.sim_hour)
            .unwrap_or(8.0);
        for tick in (from + 1)..=to {
            for (_, input) in inputs.iter().filter(|(t, _)| *t == tick) {
                match input {
                    ReplayInput::Action(a) => {
                        let _ = action_tx.send(a.clone());
                    }
                    ReplayInput::Operator(c) => {
                        let _ = operator_tx.send((**c).clone());
                    }
                }
            }
            {
                let mut time = world.resource_mut::<SimulationTime>();
                time.tick = Tick(tick);
                time.tick_count = tick;
                // #530: den in der World gesetzten delta verwenden (Default 1.0; Test kann time_scale
                // simulieren) — spiegelt den echten Live-Loop, der delta aus der Config nimmt.
                let d = time.delta_seconds;
                sim_hour = (sim_hour + d / 3600.0) % 24.0;
                time.sim_hour = sim_hour;
            }
            {
                let (cpu, mem) = psi_metrics_at(&bands, tick);
                let mut m = world.resource_mut::<PsiMetrics>();
                m.cpu_avg10 = cpu;
                m.mem_avg10 = mem;
            }
            schedule.run(world);
            while perception_rx.try_recv().is_ok() {}
            if let Some(mut active) = world.get_resource_mut::<ActiveAgentsThisTick>() {
                active.0.clear();
            }
        }
        // Scratch wieder entfernen (sonst doppelte Resource beim naechsten run_ticks).
        world.remove_resource::<ActionReceiver>();
        world.remove_resource::<OperatorCommandReceiver>();
        world.remove_resource::<PerceptionSender>();
    }
}
