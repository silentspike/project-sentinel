//! #497 AC-1 — filtered per-container ECS snapshot contains exactly one agent, no foreign state.
//!
//! The "no world-scope resources" guarantee is structural: `NanoContainerEcsSnapshot` has no
//! resource fields at all (it cannot represent ActiveChaos/Smells/RoomChat/Gaia/Broadcast). These
//! tests cover the per-agent isolation of the data path.

use sentinel_common::AgentId;
use sentinel_ecs::{create_simulation_world, snapshot_agent_ecs_state, spawn_agent};

#[test]
fn snapshot_captures_exactly_one_agent() {
    let (mut world, _schedule) = create_simulation_world();
    spawn_agent(&mut world, AgentId(1), "Alice", "Dev", 1, "buero-dev-1");
    spawn_agent(&mut world, AgentId(2), "Bob", "Dev", 1, "buero-dev-1");

    let snap_a = snapshot_agent_ecs_state(&mut world, AgentId(1)).expect("agent A exists");
    assert_eq!(
        snap_a.agent_id, 1,
        "snapshot is keyed to the requested agent"
    );
    assert_eq!(snap_a.identity.name, "Alice");
    assert_ne!(
        snap_a.identity.name, "Bob",
        "snapshot of A must not carry B's identity (no foreign state)"
    );

    let snap_b = snapshot_agent_ecs_state(&mut world, AgentId(2)).expect("agent B exists");
    assert_eq!(snap_b.agent_id, 2);
    assert_eq!(snap_b.identity.name, "Bob");

    // Two different agents serialize differently — the snapshot is per-agent, not whole-world.
    assert_ne!(
        serde_json::to_string(&snap_a).unwrap(),
        serde_json::to_string(&snap_b).unwrap(),
        "per-agent snapshots must differ between agents"
    );
}

#[test]
fn snapshot_of_unknown_agent_is_none() {
    let (mut world, _schedule) = create_simulation_world();
    spawn_agent(&mut world, AgentId(1), "Alice", "Dev", 1, "buero-dev-1");
    assert!(
        snapshot_agent_ecs_state(&mut world, AgentId(999)).is_none(),
        "snapshotting a non-existent container must return None, not a default agent"
    );
}

#[test]
fn snapshot_is_an_owned_clone_independent_of_later_world_mutation() {
    use sentinel_common::components::BioState;

    let (mut world, _schedule) = create_simulation_world();
    let a = spawn_agent(&mut world, AgentId(1), "Alice", "Dev", 1, "buero-dev-1");

    let snap = snapshot_agent_ecs_state(&mut world, AgentId(1)).expect("exists");
    let captured = snap.bio_state.energy;

    // Mutate the live agent AFTER the snapshot — the snapshot must be unaffected (owned clone).
    world.entity_mut(a).get_mut::<BioState>().unwrap().energy = -1234.0;

    assert_eq!(
        snap.bio_state.energy, captured,
        "snapshot must be an owned clone, not a live reference into the world"
    );
}
