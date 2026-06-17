//! #491 (TM-3): Deterministischer State-Hash fuer Restore/Replay-Evidence.
//!
//! Aus dem #490-Spike (`services/sentinel-daemon/src/bin/replay-spike.rs`) in die Produktion
//! gehoben, damit Spike, der `GET /operator/state-hash`-Endpunkt und kuenftige Drift-Checks EINE
//! kanonische Implementierung teilen (kein Drift). Der Spike re-importiert von hier.
//!
//! Idee: Zwei Welten sind sim-identisch, wenn ihr kanonisierter `EcsSnapshot` byte-gleich ist.
//! Nicht-deterministische Artefakte (Bevy-Allokationsreihenfolge, HashMap-Iteration, Event-UUIDs)
//! werden vor dem Hashen normalisiert; `f32` bleibt sein Bit-Muster (legacy-bincode, wie der Codec).

use crate::world::snapshot_ecs_state;
use bevy_ecs::world::World;
use sentinel_common::EcsSnapshot;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// STRICT- und CORE-Hash eines Welt-Zustands.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StateHashes {
    /// Voller kanonischer Snapshot.
    pub strict: String,
    /// Ohne `PerceptionState`/`EventQueue` — trennt Wahrnehmungstext-Luecken von Sim-Kern-Divergenz.
    pub core: String,
}

/// Re-serialisiert JSON-Bytes durch `serde_json::Value`, damit die HashMap-Schluesselordnung
/// kanonisch (BTreeMap) ist. Ungueltiges JSON bleibt unveraendert (z.B. leeres Feld).
pub fn canonical_json(bytes: &[u8]) -> Vec<u8> {
    match serde_json::from_slice::<serde_json::Value>(bytes) {
        Ok(v) => serde_json::to_vec(&v).unwrap_or_else(|_| bytes.to_vec()),
        Err(_) => bytes.to_vec(),
    }
}

/// Legacy-bincode-Encoding (wie `snapshot_codec`) — stabiles `f32`-Bit-Muster, keine FMA-Effekte.
pub fn bincode_bytes<T: Serialize>(value: &T) -> Vec<u8> {
    bincode::serde::encode_to_vec(value, bincode::config::legacy())
        .expect("bincode legacy encode of canonical snapshot")
}

/// SHA-256 als Hex (konsistent mit `hash_chain.rs`).
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    format!("{:x}", h.finalize())
}

/// Kanonisiert einen `EcsSnapshot` fuer das Hashen.
/// N1: jede Component-/Cooldown-Liste nach Agent-/Task-Schluessel sortieren (Bevy-Allokationsordnung
///     ist ueber Restore hinweg nicht stabil);
/// N2: `transit_correlation_id := None` (UUIDv4-Event-Identitaet, kein Sim-State);
/// N3: chaos/stimuli + die vier #491-Buffer-JSONs kanonisieren (HashMap-Byte-Ordnung);
/// N4: sonst nichts (`f32` bleibt Bit-Muster via legacy-bincode).
pub fn canonicalize(mut s: EcsSnapshot) -> EcsSnapshot {
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
    // #491: Cooldowns nach Agent-id sortieren (Query-Reihenfolge nicht stabil).
    s.autonomy_cooldowns.sort_by_key(|(id, _)| *id);
    // Task-Entities haben keinen Agent-u16-Schluessel — nach kanonischem bincode-Encoding ordnen.
    s.task_states.sort_by_cached_key(bincode_bytes);
    // N2: per-Action-UUID, die in den ECS-State leckt, entfernen.
    for (_, p) in s.positions.iter_mut() {
        p.transit_correlation_id = None;
    }
    // N3: HashMap-ordnungsunabhaengiges JSON (inkl. der vier #491-Buffer).
    s.active_chaos_json = canonical_json(&s.active_chaos_json);
    s.active_stimuli_json = canonical_json(&s.active_stimuli_json);
    s.smells_json = canonical_json(&s.smells_json);
    s.room_chat_json = canonical_json(&s.room_chat_json);
    s.gaia_json = canonical_json(&s.gaia_json);
    s.broadcast_json = canonical_json(&s.broadcast_json);
    s
}

/// Berechnet STRICT- und CORE-Hash der aktuellen Welt. `&mut World`, weil `snapshot_ecs_state`
/// fuer seine Queries mutablen Zugriff braucht — der Zustand wird dabei NICHT veraendert.
pub fn state_hashes(world: &mut World) -> StateHashes {
    let canon = canonicalize(snapshot_ecs_state(world));
    let strict = sha256_hex(&bincode_bytes(&canon));
    let mut core = canon;
    core.perception_states.clear();
    core.event_queues.clear();
    let core = sha256_hex(&bincode_bytes(&core));
    StateHashes { strict, core }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::create_simulation_world;

    #[test]
    fn state_hash_is_stable_for_same_world() {
        // Zwei Hashes derselben Welt sind identisch (Determinismus-Grundlage).
        let (mut world, _) = create_simulation_world();
        let h1 = state_hashes(&mut world);
        let h2 = state_hashes(&mut world);
        assert_eq!(h1, h2, "wiederholter Hash derselben Welt ist gleich");
        assert_eq!(h1.strict.len(), 64, "sha256 hex");
    }

    #[test]
    fn canonical_json_sorts_map_keys() {
        // Verschiedene Schluessel-Reihenfolge -> gleiches kanonisches JSON.
        let a = br#"{"b":1,"a":2}"#;
        let b = br#"{"a":2,"b":1}"#;
        assert_eq!(canonical_json(a), canonical_json(b));
        // Ungueltiges JSON bleibt unveraendert.
        assert_eq!(canonical_json(b"not json"), b"not json".to_vec());
    }

    #[test]
    fn canonicalize_is_order_independent_for_components() {
        // Gleiche Daten in anderer Vec-Reihenfolge -> gleicher Hash (N1).
        let (mut world, _) = create_simulation_world();
        let mut snap = snapshot_ecs_state(&mut world);
        let h_a = sha256_hex(&bincode_bytes(&canonicalize(snap.clone())));
        snap.bio_states.reverse();
        snap.identities.reverse();
        snap.autonomy_cooldowns.reverse();
        let h_b = sha256_hex(&bincode_bytes(&canonicalize(snap)));
        assert_eq!(
            h_a, h_b,
            "Reihenfolge der Component-Vecs darf den Hash nicht aendern"
        );
    }

    #[test]
    fn determinism_two_runs_identical_hash() {
        // #494 (DEV-010): zwei identische bio+ECS-Tick-Sequenzen auf derselben
        // Maschine/Binary muessen denselben STRICT- UND CORE-Hash ergeben — das
        // Intra-Run-Determinismus-Gate. Faengt HashMap-/Iterations-Nichtdeterminismus.
        // Cross-ISA-f32 ist die dokumentierte homogene-only-Grenze (DEV-010), auf
        // EINER Maschine nicht test-fangbar (siehe #406 fuer heterogene Knoten).
        fn run_sequence() -> StateHashes {
            use crate::world::{spawn_agent, SimulationTime};
            use sentinel_common::{AgentId, Tick};

            let (mut world, mut schedule) = create_simulation_world();
            for i in 1..=10u16 {
                spawn_agent(
                    &mut world,
                    AgentId(i),
                    &format!("Agent-{i:02}"),
                    "Mitarbeiter",
                    1,
                    "empfang",
                );
            }
            for tick in 0..50u64 {
                let mut time = world.resource_mut::<SimulationTime>();
                time.tick = Tick(tick);
                time.tick_count = tick;
                time.delta_seconds = 1.0;
                time.sim_hour = 8.0 + (tick as f32 / 3600.0);
                schedule.run(&mut world);
            }
            state_hashes(&mut world)
        }

        let a = run_sequence();
        let b = run_sequence();
        // Evidence (sichtbar mit `--nocapture`): der reproduzierbare DEV-010-Hash.
        eprintln!(
            "DEV-010 two-run hashes: A.strict={} B.strict={}",
            a.strict, b.strict
        );
        eprintln!(
            "DEV-010 two-run hashes: A.core={}  B.core={}",
            a.core, b.core
        );
        assert_eq!(
            a.strict, b.strict,
            "STRICT-Hash zweier identischer Laeufe muss gleich sein (DEV-010)"
        );
        assert_eq!(
            a.core, b.core,
            "CORE-Hash zweier identischer Laeufe muss gleich sein (DEV-010)"
        );
    }
}
