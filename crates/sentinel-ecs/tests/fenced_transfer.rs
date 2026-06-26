//! #497 (#8) — per-container snapshot coupled to the #489 FencedStateTransfer / #496 owner fence.

use sentinel_common::feature_flags::RuntimeFlags;
use sentinel_common::{AgentId, NotMigratableReason, OwnerRegistry, StateTransferScope};
use sentinel_ecs::{
    create_simulation_world, fenced_per_container_snapshot, spawn_agent, RoomChatBuffer,
};

#[test]
fn fenced_snapshot_is_taken_under_the_owner_fence() {
    let (mut world, _) = create_simulation_world();
    spawn_agent(&mut world, AgentId(1), "Alice", "Dev", 1, "buero-dev-1");

    let snap = fenced_per_container_snapshot(&mut world, AgentId(1)).expect("resting -> Ok");
    assert_eq!(snap.agent_id, 1);

    // The cut's owner_epoch is the #496 fence epoch for this container's NanoContainer scope.
    let scope = StateTransferScope::for_agent(AgentId(1).to_string());
    let reg = OwnerRegistry::global();
    assert_eq!(snap.cut.owner_epoch, reg.current_owner(&scope).epoch);

    // Single-node fence is a no-op pass: a guard for the scope validates with 0 StaleEpoch.
    let guard = reg.issue(scope.clone());
    assert!(
        reg.validate(&guard).is_ok(),
        "single-node fence must pass (0 StaleEpoch) — prod path unchanged"
    );
}

#[test]
fn fenced_snapshot_respects_eligibility() {
    let (mut world, _) = create_simulation_world();
    spawn_agent(&mut world, AgentId(1), "Alice", "Dev", 1, "buero-dev-1");
    spawn_agent(&mut world, AgentId(2), "Bob", "Dev", 1, "buero-dev-1");
    // Bob addresses Alice → Alice has active inbound → NotMigratable, never a silent skip.
    world.resource_mut::<RoomChatBuffer>().add(
        "buero-dev-1",
        "Bob".to_string(),
        "Hey Alice, ready?".to_string(),
        0,
        &["Alice".to_string(), "Bob".to_string()],
    );

    assert_eq!(
        fenced_per_container_snapshot(&mut world, AgentId(1)).unwrap_err(),
        NotMigratableReason::ActiveInbound
    );
}

#[test]
fn to_fenced_transfer_carries_scope_and_owner_epoch() {
    let (mut world, _) = create_simulation_world();
    spawn_agent(&mut world, AgentId(1), "Alice", "Dev", 1, "buero-dev-1");
    let snap = fenced_per_container_snapshot(&mut world, AgentId(1)).unwrap();

    let transfer = snap.to_fenced_transfer();
    assert_eq!(
        transfer.scope,
        StateTransferScope::for_agent(AgentId(1).to_string()),
        "transfer is scoped to the NanoContainer, not World"
    );
    assert!(matches!(
        transfer.scope,
        StateTransferScope::NanoContainer(_)
    ));
    assert_eq!(
        transfer.owner_epoch, snap.cut.owner_epoch,
        "the #489 envelope carries the #496 fence epoch"
    );
}

#[test]
fn per_container_transfer_flag_defaults_off() {
    assert!(
        !RuntimeFlags::global().per_container_transfer_enabled,
        "Strangler: per-container transfer must be OFF by default (single-node prod unchanged)"
    );
}
