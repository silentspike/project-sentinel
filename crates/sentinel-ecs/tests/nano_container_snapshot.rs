//! #497 AC-1 — filtered per-container ECS snapshot contains exactly one agent, no foreign state.
//!
//! The "no world-scope resources" guarantee is structural: `NanoContainerEcsSnapshot` has no
//! resource fields at all (it cannot represent ActiveChaos/Smells/RoomChat/Gaia/Broadcast). These
//! tests cover the per-agent isolation of the data path.

use sentinel_common::AgentId;
use sentinel_ecs::{
    create_simulation_world, restore_agent_ecs_state, snapshot_agent_ecs_state, spawn_agent,
};

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

#[test]
fn restore_replaces_only_the_one_agent() {
    use sentinel_common::components::{AgentIdentity, BioState};

    let (mut world, _sched) = create_simulation_world();
    spawn_agent(&mut world, AgentId(1), "Alice", "Dev", 1, "buero-dev-1");
    spawn_agent(&mut world, AgentId(2), "Bob", "Dev", 1, "buero-dev-1");

    // Snapshot A, then corrupt A's live state, and record B's state before the restore.
    let snap_a = snapshot_agent_ecs_state(&mut world, AgentId(1)).unwrap();
    {
        let mut q = world.query::<(&AgentIdentity, &mut BioState)>();
        for (id, mut bio) in q.iter_mut(&mut world) {
            if id.agent_id == AgentId(1) {
                bio.energy = -999.0;
            }
        }
    }
    let snap_b_before = snapshot_agent_ecs_state(&mut world, AgentId(2)).unwrap();

    // Restore ONLY agent A.
    restore_agent_ecs_state(&mut world, &snap_a);

    // A is back to its snapshot (the corruption is gone).
    let snap_a_after = snapshot_agent_ecs_state(&mut world, AgentId(1)).unwrap();
    assert_eq!(
        serde_json::to_string(&snap_a).unwrap(),
        serde_json::to_string(&snap_a_after).unwrap(),
        "restored agent must match its snapshot"
    );

    // B is untouched by the per-container restore.
    let snap_b_after = snapshot_agent_ecs_state(&mut world, AgentId(2)).unwrap();
    assert_eq!(
        serde_json::to_string(&snap_b_before).unwrap(),
        serde_json::to_string(&snap_b_after).unwrap(),
        "other agents must be untouched by a per-container restore"
    );

    // Exactly one entity carries agent_id 1 — no orphan, no duplicate after despawn+respawn.
    let mut count = 0;
    let mut q2 = world.query::<&AgentIdentity>();
    for id in q2.iter(&world) {
        if id.agent_id == AgentId(1) {
            count += 1;
        }
    }
    assert_eq!(
        count, 1,
        "despawn+respawn must leave exactly one agent-1 entity (no orphan / no duplicate)"
    );
}

/// AC-3b / V12: after a per-container restore (new EntityId), a holder still reaches the agent —
/// because resolution goes `agent_id` → RouteRegistry → live entity-by-agent_id, never a cached
/// `EntityId`. A cached handle would be stale; agent_id resolution finds the NEW entity.
#[test]
fn reference_integrity_after_restore_via_route_registry() {
    use bevy_ecs::prelude::{Entity, World};
    use sentinel_common::cluster::NodeId;
    use sentinel_common::components::AgentIdentity;
    use sentinel_common::route::{RouteRegistry, RouteState};

    let (mut world, _sched) = create_simulation_world();
    spawn_agent(&mut world, AgentId(1), "Alice", "Dev", 1, "buero-dev-1");
    spawn_agent(&mut world, AgentId(2), "Bob", "Dev", 1, "buero-dev-1");

    let reg = RouteRegistry::new();
    reg.register(1, NodeId(uuid::Uuid::nil()), 1, RouteState::Local);

    // Resolve A the only correct way: by agent_id via the registry, then a fresh entity lookup.
    let resolve_a = |world: &mut World| -> Entity {
        assert_eq!(
            reg.resolve(1).expect("A registered").state,
            RouteState::Local,
            "A is local"
        );
        let mut q = world.query::<(Entity, &AgentIdentity)>();
        q.iter(world)
            .find(|(_, id)| id.agent_id == AgentId(1))
            .map(|(e, _)| e)
            .expect("A entity exists")
    };

    let before = resolve_a(&mut world);

    // Restore A → despawn+respawn → a NEW EntityId.
    let snap = snapshot_agent_ecs_state(&mut world, AgentId(1)).unwrap();
    restore_agent_ecs_state(&mut world, &snap);

    // A is STILL reachable — agent_id resolution finds the new entity, not a stale handle.
    let after = resolve_a(&mut world);
    assert_ne!(
        before, after,
        "restore yields a new EntityId (despawn+respawn) — a cached EntityId would be stale here"
    );

    // Exactly one agent-1 entity remains: no orphan the resolver could trip over.
    let mut count = 0;
    let mut q = world.query::<&AgentIdentity>();
    for id in q.iter(&world) {
        if id.agent_id == AgentId(1) {
            count += 1;
        }
    }
    assert_eq!(count, 1);
}
