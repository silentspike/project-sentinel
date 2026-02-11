//! Acceptance Tests fuer Issue #15: sentinel-runtime
//!
//! Testet RuntimeOrchestrator: spawn/despawn, max-agents-limit,
//! Schichtwechsel, Sonder-Set Beibehaltung, Health-Checks.

use sentinel_common::components::{AgentIdentity, ShiftInfo};
use sentinel_common::AgentId;
use sentinel_runtime::{AgentStatus, RuntimeOrchestrator};

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

// AC #15.02: spawn, verify active, despawn, verify gone
#[test]
fn ac_15_02_spawn_despawn() {
    let mut orch = RuntimeOrchestrator::new(10);

    let identity = create_identity(1, "Thomas", "CEO");
    let shift = create_shift(1, 6, 14);

    // Spawn
    orch.spawn_agent(identity, shift).unwrap();
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

// AC #15.03: spawn max+1 -> Error
#[test]
fn ac_15_03_max_agents_limit() {
    let mut orch = RuntimeOrchestrator::new(2);

    orch.spawn_agent(create_identity(1, "Thomas", "CEO"), create_shift(1, 6, 14))
        .unwrap();
    orch.spawn_agent(
        create_identity(2, "Lisa", "Designer"),
        create_shift(1, 6, 14),
    )
    .unwrap();

    // Dritter Agent ueberschreitet max_agents=2
    let result = orch.spawn_agent(
        create_identity(3, "Andreas", "Developer"),
        create_shift(1, 6, 14),
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

// AC #15.04: Set 1 active, transition to Set 2, verify Set 1 gone
#[test]
fn ac_15_04_shift_transition() {
    let mut orch = RuntimeOrchestrator::new(20);

    // Set 1 Agents (Frueh-Schicht)
    orch.spawn_agent(create_identity(1, "Thomas", "CEO"), create_shift(1, 6, 14))
        .unwrap();
    orch.spawn_agent(
        create_identity(2, "Lisa", "Designer"),
        create_shift(1, 6, 14),
    )
    .unwrap();

    // Set 2 Agent (Mittel-Schicht)
    orch.spawn_agent(
        create_identity(16, "Michael", "CEO"),
        create_shift(2, 14, 22),
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
    )
    .unwrap();

    // Set 1 Agent
    orch.spawn_agent(create_identity(1, "Thomas", "CEO"), create_shift(1, 6, 14))
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

// AC #15.06: Agent auf Errored setzen, check_health() findet ihn
#[test]
fn ac_15_06_health_check() {
    let mut orch = RuntimeOrchestrator::new(10);

    orch.spawn_agent(create_identity(1, "Thomas", "CEO"), create_shift(1, 6, 14))
        .unwrap();
    orch.spawn_agent(
        create_identity(2, "Lisa", "Designer"),
        create_shift(1, 6, 14),
    )
    .unwrap();

    // Setze Agent 1 auf Errored
    if let Some(handle) = orch.get_agent_mut(AgentId(1)) {
        handle.status = AgentStatus::Errored;
    }

    let unhealthy = orch.check_health();

    assert_eq!(
        unhealthy.len(),
        1,
        "Exactly one agent should be unhealthy"
    );
    assert_eq!(unhealthy[0].0, AgentId(1), "Unhealthy agent should be #1");
    assert_eq!(
        unhealthy[0].1,
        AgentStatus::Errored,
        "Status should be Errored"
    );
}
