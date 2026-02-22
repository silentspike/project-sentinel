//! Acceptance Tests fuer Issue #15: sentinel-runtime
//!
//! Testet RuntimeOrchestrator: spawn/despawn, max-agents-limit,
//! Schichtwechsel, Sonder-Set Beibehaltung, Health-Checks,
//! Event-Sourced Lifecycle (AC-2), Resume nach Neustart (AC-4).

use std::sync::Arc;

use sentinel_common::components::{AgentIdentity, ShiftInfo};
use sentinel_common::AgentId;
use sentinel_limbo::EventStore;
use sentinel_runtime::{AgentStatus, RuntimeOrchestrator};
use tempfile::TempDir;

fn create_identity(id: u16, name: &str, role: &str) -> AgentIdentity {
    AgentIdentity {
        agent_id: AgentId(id),
        name: name.to_string(),
        role: role.to_string(),
    }
}

fn create_shift(shift_set: u8, start: u8, end: u8) -> ShiftInfo {
    ShiftInfo {
        shift_set,
        shift_start_hour: start,
        shift_end_hour: end,
        is_on_duty: true,
    }
}

fn temp_event_store() -> (TempDir, Arc<EventStore>) {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("acceptance_runtime.db");
    let store = EventStore::open(path.to_str().unwrap()).unwrap();
    (dir, Arc::new(store))
}

// AC #15.02: spawn, verify active, despawn, verify gone
#[test]
fn ac_15_02_spawn_despawn() {
    let mut orch = RuntimeOrchestrator::new(10);

    let identity = create_identity(1, "Thomas", "CEO");
    let shift = create_shift(1, 6, 14);

    // Spawn
    orch.spawn_agent(identity, shift, "empfang").unwrap();
    assert_eq!(orch.agent_count(), 1, "After spawn: 1 agent expected");

    // Verify active (get_agent_mut returns Some)
    let handle = orch.get_agent_mut(AgentId(1));
    assert!(handle.is_some(), "Agent 1 should be accessible after spawn");
    assert_eq!(
        handle.unwrap().status,
        AgentStatus::Active,
        "Freshly spawned agent should be Active"
    );

    // Despawn
    orch.despawn_agent(AgentId(1)).unwrap();
    assert_eq!(orch.agent_count(), 0, "After despawn: 0 agents expected");

    // Verify gone
    assert!(
        orch.get_agent_mut(AgentId(1)).is_none(),
        "Agent 1 should not be accessible after despawn"
    );
}

// AC #15.02b: Lifecycle events are event-sourced (AC-2)
#[test]
fn ac_15_02_lifecycle_events_sourced() {
    let (_dir, store) = temp_event_store();
    let mut orch = RuntimeOrchestrator::new(20).with_event_store(store.clone());
    orch.set_tick(1);

    // Spawn 3 agents
    orch.spawn_agent(
        create_identity(1, "Thomas", "CEO"),
        create_shift(1, 6, 14),
        "empfang",
    )
    .unwrap();
    orch.spawn_agent(
        create_identity(2, "Lisa", "Designer"),
        create_shift(1, 6, 14),
        "empfang",
    )
    .unwrap();
    orch.spawn_agent(
        create_identity(3, "Andreas", "Developer"),
        create_shift(1, 6, 14),
        "empfang",
    )
    .unwrap();

    // Verify spawn events in store
    let events_1 = store.get_events_by_aggregate("AGENT-01", 10).unwrap();
    assert_eq!(events_1.len(), 1, "Agent 1 should have 1 spawn event");
    assert_eq!(events_1[0].event_type, "agent_spawned");

    let events_2 = store.get_events_by_aggregate("AGENT-02", 10).unwrap();
    assert_eq!(events_2.len(), 1, "Agent 2 should have 1 spawn event");

    // Despawn Agent 2
    orch.despawn_agent(AgentId(2)).unwrap();

    let events_2 = store.get_events_by_aggregate("AGENT-02", 10).unwrap();
    assert_eq!(
        events_2.len(),
        2,
        "Agent 2 should have 2 events (spawn + despawn)"
    );
    assert_eq!(events_2[1].event_type, "agent_despawned");

    // Shift transition
    orch.set_tick(10);
    let _ = orch.shift_transition(2);

    let runtime_events = store.get_events_by_aggregate("runtime", 10).unwrap();
    assert_eq!(
        runtime_events.len(),
        1,
        "Runtime should have 1 shift transition event"
    );
    assert_eq!(
        runtime_events[0].event_type, "shift_transition_completed",
        "Event type should be shift_transition_completed"
    );
    assert_eq!(runtime_events[0].tick, 10, "Event tick should match");

    // All events have outbox entries (for Zenoh)
    // Total events: 3 spawns + 1 despawn + 1 shift = 5
    let all_events = store.get_events_since(0, 100).unwrap();
    assert!(
        all_events.len() >= 5,
        "Should have at least 5 lifecycle events, got {}",
        all_events.len()
    );
}

// AC #15.03: spawn max+1 -> Error
#[test]
fn ac_15_03_max_agents_limit() {
    let mut orch = RuntimeOrchestrator::new(2);

    orch.spawn_agent(
        create_identity(1, "Thomas", "CEO"),
        create_shift(1, 6, 14),
        "empfang",
    )
    .unwrap();
    orch.spawn_agent(
        create_identity(2, "Lisa", "Designer"),
        create_shift(1, 6, 14),
        "empfang",
    )
    .unwrap();

    // Dritter Agent ueberschreitet max_agents=2
    let result = orch.spawn_agent(
        create_identity(3, "Andreas", "Developer"),
        create_shift(1, 6, 14),
        "empfang",
    );

    assert!(
        result.is_err(),
        "Spawning beyond max_agents limit should return an error"
    );
    assert_eq!(
        orch.agent_count(),
        2,
        "Agent count should remain at max after rejected spawn"
    );
}

// AC #15.04: Resume after restart with persisted states
#[test]
fn ac_15_04_resume_after_restart() {
    let (_dir, store) = temp_event_store();

    // Phase 1: Create orchestrator, spawn agents, save state
    {
        let mut orch = RuntimeOrchestrator::new(20).with_event_store(store.clone());
        orch.set_tick(50);

        // Spawn 5 agents across shifts
        orch.spawn_agent(
            create_identity(1, "Thomas", "CEO"),
            create_shift(1, 6, 14),
            "empfang",
        )
        .unwrap();
        orch.spawn_agent(
            create_identity(2, "Lisa", "Designer"),
            create_shift(1, 6, 14),
            "empfang",
        )
        .unwrap();
        orch.spawn_agent(
            create_identity(3, "Andreas", "Developer"),
            create_shift(2, 14, 22),
            "empfang",
        )
        .unwrap();
        orch.spawn_agent(
            create_identity(4, "Sandra", "PM"),
            create_shift(2, 14, 22),
            "empfang",
        )
        .unwrap();
        orch.spawn_agent(
            create_identity(46, "Betriebsrat", "Sonder"),
            create_shift(0, 0, 23),
            "empfang",
        )
        .unwrap();

        // Set one to Errored
        if let Some(h) = orch.get_agent_mut(AgentId(3)) {
            h.status = AgentStatus::Errored;
        }

        assert_eq!(orch.agent_count(), 5);

        // Save state (simulates clean shutdown)
        orch.save_state().unwrap();
    }
    // Orchestrator dropped here — simulates process exit

    // Phase 2: Restore from snapshot (simulates restart)
    let mut restored = RuntimeOrchestrator::restore(store, 20).unwrap();

    // Verify all 5 agents are back
    assert_eq!(
        restored.agent_count(),
        5,
        "All 5 agents should be restored after restart"
    );

    // Verify specific agent identities survived
    let agent1 = restored.get_agent_mut(AgentId(1)).unwrap();
    assert_eq!(agent1.identity.name, "Thomas");
    assert_eq!(agent1.identity.role, "CEO");
    assert_eq!(agent1.status, AgentStatus::Active);
    assert_eq!(agent1.shift.shift_set, 1);

    // Verify Errored status persisted
    let agent3 = restored.get_agent_mut(AgentId(3)).unwrap();
    assert_eq!(
        agent3.status,
        AgentStatus::Errored,
        "Errored status should persist across restart"
    );
    assert_eq!(agent3.identity.name, "Andreas");

    // Verify Sonder-Agent persisted
    let sonder = restored.get_agent_mut(AgentId(46)).unwrap();
    assert_eq!(sonder.shift.shift_set, 0, "Sonder shift_set should be 0");
    assert_eq!(sonder.identity.name, "Betriebsrat");
}

// AC #15.04b: Set 1 active, transition to Set 2, verify Set 1 gone
#[test]
fn ac_15_04b_shift_transition() {
    let mut orch = RuntimeOrchestrator::new(20);

    // Set 1 Agents (Frueh-Schicht)
    orch.spawn_agent(
        create_identity(1, "Thomas", "CEO"),
        create_shift(1, 6, 14),
        "empfang",
    )
    .unwrap();
    orch.spawn_agent(
        create_identity(2, "Lisa", "Designer"),
        create_shift(1, 6, 14),
        "empfang",
    )
    .unwrap();

    // Set 2 Agent (Mittel-Schicht)
    orch.spawn_agent(
        create_identity(16, "Michael", "CEO"),
        create_shift(2, 14, 22),
        "empfang",
    )
    .unwrap();

    assert_eq!(orch.agent_count(), 3);

    // Transition zu Set 2
    let removed = orch.shift_transition(2);

    // Set 1 Agents (1, 2) sollten entfernt worden sein
    assert_eq!(
        removed.len(),
        2,
        "Two Set-1 agents should have been removed"
    );
    assert!(
        orch.get_agent_mut(AgentId(1)).is_none(),
        "Agent 1 (Set 1) should be gone after transition to Set 2"
    );
    assert!(
        orch.get_agent_mut(AgentId(2)).is_none(),
        "Agent 2 (Set 1) should be gone after transition to Set 2"
    );

    // Set 2 Agent sollte noch da sein
    assert!(
        orch.get_agent_mut(AgentId(16)).is_some(),
        "Agent 16 (Set 2) should remain after transition to Set 2"
    );
    assert_eq!(orch.agent_count(), 1);
}

// AC #15.05: Set 0 (Sonder) Agents bleiben nach Transition erhalten
#[test]
fn ac_15_05_sonder_set_preserved() {
    let mut orch = RuntimeOrchestrator::new(20);

    // Sonder-Agent (Set 0)
    orch.spawn_agent(
        create_identity(46, "Betriebsrat", "Sonder"),
        create_shift(0, 0, 23),
        "empfang",
    )
    .unwrap();

    // Set 1 Agent
    orch.spawn_agent(
        create_identity(1, "Thomas", "CEO"),
        create_shift(1, 6, 14),
        "empfang",
    )
    .unwrap();

    assert_eq!(orch.agent_count(), 2);

    // Transition zu Set 2
    let removed = orch.shift_transition(2);

    // Nur Set 1 Agent sollte entfernt werden
    assert_eq!(removed.len(), 1, "Only Set-1 agent should be removed");
    assert!(
        removed.contains(&AgentId(1)),
        "Removed list should contain Agent 1 (Set 1)"
    );

    // Sonder-Agent sollte noch da sein
    assert!(
        orch.get_agent_mut(AgentId(46)).is_some(),
        "Sonder-Agent (Set 0) must survive shift transitions"
    );
    assert_eq!(orch.agent_count(), 1);
}

// AC #15.07: pause_agent() und resume_agent() mit State-Machine-Validierung
#[test]
fn ac_15_07_pause_resume_lifecycle() {
    let (_dir, store) = temp_event_store();
    let mut orch = RuntimeOrchestrator::new(20).with_event_store(store.clone());
    orch.set_tick(1);

    orch.spawn_agent(
        create_identity(1, "Thomas", "CEO"),
        create_shift(1, 6, 14),
        "empfang",
    )
    .unwrap();
    orch.spawn_agent(
        create_identity(2, "Lisa", "Designer"),
        create_shift(1, 6, 14),
        "empfang",
    )
    .unwrap();

    // Pause Agent 1: Active -> Suspended
    orch.pause_agent(AgentId(1)).unwrap();
    assert_eq!(
        orch.get_agent_mut(AgentId(1)).unwrap().status,
        AgentStatus::Suspended,
        "Agent 1 should be Suspended after pause"
    );

    // Agent 2 bleibt Active
    assert_eq!(
        orch.get_agent_mut(AgentId(2)).unwrap().status,
        AgentStatus::Active,
        "Agent 2 should still be Active"
    );

    // Health-Check: Suspended Agent taucht auf
    let unhealthy = orch.check_health();
    assert_eq!(unhealthy.len(), 1);
    assert_eq!(unhealthy[0].1, AgentStatus::Suspended);

    // Resume Agent 1: Suspended -> Active
    orch.resume_agent(AgentId(1)).unwrap();
    assert_eq!(
        orch.get_agent_mut(AgentId(1)).unwrap().status,
        AgentStatus::Active,
        "Agent 1 should be Active after resume"
    );

    // Invalid: Pause already-Suspended -> Error
    orch.pause_agent(AgentId(1)).unwrap();
    assert!(
        orch.pause_agent(AgentId(1)).is_err(),
        "Double-pause should fail (Suspended -> Suspended invalid)"
    );

    // Invalid: Resume already-Active -> Error
    orch.resume_agent(AgentId(1)).unwrap();
    assert!(
        orch.resume_agent(AgentId(1)).is_err(),
        "Resume Active agent should fail (Active -> Active invalid)"
    );

    // Verify events in store: spawn(1) + spawn(2) + pause + resume + pause + resume = 6 agent events
    let events_1 = store.get_events_by_aggregate("AGENT-01", 20).unwrap();
    assert!(
        events_1.len() >= 5,
        "Agent 1 should have >= 5 events (spawn + 2x pause + 2x resume), got {}",
        events_1.len()
    );

    // Check status_changed events are present
    let status_events: Vec<_> = events_1
        .iter()
        .filter(|e| e.event_type == "agent_status_changed")
        .collect();
    assert!(
        status_events.len() >= 4,
        "Should have >= 4 status_changed events, got {}",
        status_events.len()
    );
}

// AC #15.06: Agent auf Errored setzen, check_health() findet ihn
#[test]
fn ac_15_06_health_check() {
    let mut orch = RuntimeOrchestrator::new(10);

    orch.spawn_agent(
        create_identity(1, "Thomas", "CEO"),
        create_shift(1, 6, 14),
        "empfang",
    )
    .unwrap();
    orch.spawn_agent(
        create_identity(2, "Lisa", "Designer"),
        create_shift(1, 6, 14),
        "empfang",
    )
    .unwrap();

    // Setze Agent 1 auf Errored
    if let Some(handle) = orch.get_agent_mut(AgentId(1)) {
        handle.status = AgentStatus::Errored;
    }

    let unhealthy = orch.check_health();

    assert_eq!(unhealthy.len(), 1, "Exactly one agent should be unhealthy");
    assert_eq!(unhealthy[0].0, AgentId(1), "Unhealthy agent should be #1");
    assert_eq!(
        unhealthy[0].1,
        AgentStatus::Errored,
        "Status should be Errored"
    );
}
