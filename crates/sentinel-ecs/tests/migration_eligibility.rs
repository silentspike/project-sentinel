//! #497 (V11/V23/V30) — bounded migration class: every rejection is a TYPED reason, never a silent
//! skip. A resting container is eligible; active inbound, scheduled work, or a pending side-effect
//! each produce a distinct typed `NotMigratableReason`.

use sentinel_common::components::TaskState;
use sentinel_common::{AgentId, MigrationEligibility, NotMigratableReason, TaskId, TaskStatus};
use sentinel_ecs::{
    create_simulation_world, migration_eligibility, spawn_agent, GaiaBuffer, RoomChatBuffer,
};

#[test]
fn resting_container_is_eligible() {
    let (mut world, _) = create_simulation_world();
    spawn_agent(&mut world, AgentId(1), "Alice", "Dev", 1, "buero-dev-1");
    assert_eq!(
        migration_eligibility(&mut world, AgentId(1)),
        MigrationEligibility::Eligible
    );
}

#[test]
fn unknown_agent_is_typed_not_silent() {
    let (mut world, _) = create_simulation_world();
    assert_eq!(
        migration_eligibility(&mut world, AgentId(7)),
        MigrationEligibility::NotMigratable(NotMigratableReason::UnknownAgent)
    );
}

#[test]
fn active_inbound_blocks_migration() {
    let (mut world, _) = create_simulation_world();
    spawn_agent(&mut world, AgentId(1), "Alice", "Dev", 1, "buero-dev-1");
    spawn_agent(&mut world, AgentId(2), "Bob", "Dev", 1, "buero-dev-1");

    // Bob addresses Alice → active inbound directed at Alice (V11).
    world.resource_mut::<RoomChatBuffer>().add(
        "buero-dev-1",
        "Bob".to_string(),
        "Hey Alice, can you review this?".to_string(),
        0,
        &["Alice".to_string(), "Bob".to_string()],
    );

    assert_eq!(
        migration_eligibility(&mut world, AgentId(1)),
        MigrationEligibility::NotMigratable(NotMigratableReason::ActiveInbound)
    );
    // The sender (Bob) is not addressed — he stays eligible.
    assert!(migration_eligibility(&mut world, AgentId(2)).is_eligible());
}

#[test]
fn pending_gaia_thought_blocks_migration() {
    let (mut world, _) = create_simulation_world();
    spawn_agent(&mut world, AgentId(1), "Alice", "Dev", 1, "buero-dev-1");
    world
        .resource_mut::<GaiaBuffer>()
        .add(AgentId(1), "You must act now".to_string(), 0);

    assert_eq!(
        migration_eligibility(&mut world, AgentId(1)),
        MigrationEligibility::NotMigratable(NotMigratableReason::PendingSideEffect)
    );
}

#[test]
fn active_scheduled_task_blocks_migration() {
    let (mut world, _) = create_simulation_world();
    spawn_agent(&mut world, AgentId(1), "Alice", "Dev", 1, "buero-dev-1");
    world.spawn(TaskState {
        task_id: TaskId(42),
        title: "review".into(),
        description: "d".into(),
        assigned_to: AgentId(1),
        assigned_by: None,
        parent_task: None,
        status: TaskStatus::InProgress,
        created_tick: 0,
        updated_tick: 0,
        result: None,
    });

    assert_eq!(
        migration_eligibility(&mut world, AgentId(1)),
        MigrationEligibility::NotMigratable(NotMigratableReason::ScheduledWorkActive)
    );
}
