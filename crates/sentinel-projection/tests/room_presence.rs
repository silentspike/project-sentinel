//! Synthetic EventStore/worker regressions, not evidence from a live company.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use rusqlite::{types::Value, Connection};
use sentinel_common::{AgentId, DomainEvent, DomainEventPayload, EventType};
use sentinel_limbo::{EventStore, LegacyEventProducer};
use sentinel_projection::store::AgentView;
use sentinel_projection::worker::ROOM_IDS;
use sentinel_projection::{ProjectionConfig, ProjectionWorker};

const PROJECTION: &str = "sentinel-projection";

fn config(path: &Path) -> ProjectionConfig {
    ProjectionConfig {
        db_path: path.to_string_lossy().into_owned(),
        batch_size: 128,
        poll_interval: Duration::from_millis(1),
        rebuild_request_path: path
            .with_extension("rebuild-request")
            .to_string_lossy()
            .into_owned(),
        rebuild_request_poll_interval: Duration::from_secs(1),
    }
}

fn rows(path: &Path, sql: &str) -> Vec<Vec<Value>> {
    let connection = Connection::open(path).unwrap();
    let mut statement = connection.prepare(sql).unwrap();
    let columns = statement.column_count();
    let result = statement
        .query_map([], |row| {
            (0..columns).map(|column| row.get(column)).collect()
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    result
}

fn watermark(path: &Path) -> i64 {
    Connection::open(path)
        .unwrap()
        .query_row(
            "SELECT last_event_id FROM projection_watermarks WHERE projection_name=?1",
            [PROJECTION],
            |row| row.get(0),
        )
        .unwrap()
}

fn stable_views(path: &Path) -> Vec<Vec<Vec<Value>>> {
    [
        "SELECT * FROM agent_live_view ORDER BY agent_id",
        "SELECT room_id,occupant_count,transit_count,active_chaos,active_smells,
                temperature,co2_ppm,noise_db,last_event_tick,last_event_id
         FROM room_live_view ORDER BY room_id",
        "SELECT * FROM kpi_1m ORDER BY bucket_start",
        "SELECT projection_name,last_event_id FROM projection_watermarks ORDER BY projection_name",
    ]
    .into_iter()
    .map(|sql| rows(path, sql))
    .collect()
}

struct Fixture {
    events: Arc<EventStore>,
    worker: ProjectionWorker,
    event_path: PathBuf,
    projection_path: PathBuf,
    tick: u64,
    _temp: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let event_path = temp.path().join("events.sqlite");
        let projection_path = temp.path().join("projection.sqlite");
        let events = Arc::new(EventStore::open(event_path.to_str().unwrap()).unwrap());
        let worker = ProjectionWorker::new(Arc::clone(&events), config(&projection_path)).unwrap();
        Self {
            events,
            worker,
            event_path,
            projection_path,
            tick: 0,
            _temp: temp,
        }
    }

    fn append(&mut self, payload: DomainEventPayload) -> (i64, DomainEvent) {
        self.tick += 1;
        let mut event = DomainEvent::new(
            payload.event_type_str(),
            "room-presence-fixture",
            &payload.to_json(),
            "room-presence-regression",
            self.tick,
        );
        event.timestamp_ms = self.tick * 1_000;
        let row = self
            .events
            .legacy_append_gateway(LegacyEventProducer::TestHarness)
            .append_event(&event)
            .unwrap();
        (row, event)
    }

    fn project(&mut self, payload: DomainEventPayload) -> i64 {
        let (row, _) = self.append(payload);
        assert_eq!(self.worker.process_pending_batch().unwrap(), 1);
        assert_eq!(watermark(&self.projection_path), row);
        assert_eq!(self.events.get_offset(PROJECTION).unwrap(), Some(row));
        self.assert_derived_counts();
        row
    }

    fn agent(&self, id: u16) -> AgentView {
        self.worker.read_store().get_agent(id).unwrap().unwrap()
    }

    fn counts(&self, room: &str, occupants: i64, transits: i64) {
        let projected = self.worker.read_store().get_room(room).unwrap().unwrap();
        assert_eq!(
            (projected.occupant_count, projected.transit_count),
            (occupants, transits),
            "{room}",
        );
    }

    fn assert_derived_counts(&self) {
        let active = self.worker.read_store().active_agents().unwrap();
        for room in ROOM_IDS {
            let occupants = active
                .iter()
                .filter(|agent| !agent.in_transit && agent.current_room.as_deref() == Some(*room))
                .count() as i64;
            let transits = active
                .iter()
                .filter(|agent| agent.in_transit && agent.transit_target.as_deref() == Some(*room))
                .count() as i64;
            self.counts(room, occupants, transits);
        }
    }

    fn history(&self) -> serde_json::Value {
        serde_json::to_value(self.events.get_events_since_with_id(0, 1_000).unwrap()).unwrap()
    }

    fn reopen(self) -> Self {
        let Self {
            events,
            worker,
            event_path,
            projection_path,
            tick,
            _temp,
        } = self;
        drop(worker);
        let worker = ProjectionWorker::new(Arc::clone(&events), config(&projection_path)).unwrap();
        Self {
            events,
            worker,
            event_path,
            projection_path,
            tick,
            _temp,
        }
    }
}

fn spawn(agent: u16, room: &str) -> DomainEventPayload {
    DomainEventPayload::AgentSpawned {
        agent_id: AgentId(agent),
        name: format!("Fixture agent {agent}"),
        role: "Developer".into(),
        shift_set: 1,
        room_id: room.into(),
    }
}

fn start(agent: u16, from: &str, to: &str) -> DomainEventPayload {
    DomainEventPayload::TransitStarted {
        agent_id: AgentId(agent),
        from_room: from.into(),
        to_room: to.into(),
        duration_ms: 5_000,
    }
}

fn arrive(agent: u16, room: &str) -> DomainEventPayload {
    DomainEventPayload::TransitCompleted {
        agent_id: AgentId(agent),
        room_id: room.into(),
    }
}

fn despawn(agent: u16) -> DomainEventPayload {
    DomainEventPayload::AgentDespawned {
        agent_id: AgentId(agent),
        reason: "fixture removal".into(),
    }
}

fn status(agent: u16, old: &str, new: &str) -> DomainEventPayload {
    DomainEventPayload::AgentStatusChanged {
        agent_id: AgentId(agent),
        old_status: old.into(),
        new_status: new.into(),
    }
}

#[test]
fn initial_spawn_and_respawn_project_one_complete_room_presence() {
    let mut fixture = Fixture::new();
    let row = fixture.project(spawn(36, "empfang"));
    let agent = fixture.agent(36);
    assert_eq!(agent.current_room.as_deref(), Some("empfang"));
    assert_eq!(agent.status, "active");
    assert!(!agent.in_transit);
    assert!(agent.transit_target.is_none());
    assert_eq!(agent.last_event_id, row);
    fixture.counts("empfang", 1, 0);

    fixture.project(spawn(36, "empfang"));
    fixture.counts("empfang", 1, 0);
    fixture.project(spawn(36, "kueche"));
    fixture.counts("empfang", 0, 0);
    fixture.counts("kueche", 1, 0);

    fixture.project(start(36, "kueche", "buero-dev-1"));
    fixture.counts("kueche", 0, 0);
    fixture.counts("buero-dev-1", 0, 1);
    let row = fixture.project(spawn(36, "empfang"));
    fixture.counts("empfang", 1, 0);
    fixture.counts("buero-dev-1", 0, 0);
    let agent = fixture.agent(36);
    assert_eq!(agent.current_room.as_deref(), Some("empfang"));
    assert!(!agent.in_transit);
    assert!(agent.transit_target.is_none());
    assert_eq!(agent.last_event_id, row);

    fixture.project(despawn(36));
    fixture.counts("empfang", 0, 0);
    fixture.project(spawn(36, "kueche"));
    fixture.counts("empfang", 0, 0);
    fixture.counts("kueche", 1, 0);
}

#[test]
fn transit_arrival_updates_both_counts_at_one_row_and_duplicates_do_not_drift() {
    for destination in ["kueche", "empfang"] {
        let mut fixture = Fixture::new();
        fixture.project(spawn(36, "empfang"));
        fixture.project(spawn(39, "empfang"));
        fixture.project(start(36, "empfang", destination));
        let same_room = if destination == "empfang" { 1 } else { 0 };
        fixture.counts("empfang", 1, same_room);
        fixture.counts(destination, same_room, 1);
        fixture.project(start(36, "empfang", destination));
        fixture.counts(destination, same_room, 1);
        let row = fixture.project(arrive(36, destination));
        fixture.counts(destination, if destination == "empfang" { 2 } else { 1 }, 0);
        let agent = fixture.agent(36);
        assert_eq!(agent.last_event_id, row);
        assert_eq!(agent.current_room.as_deref(), Some(destination));
        assert!(!agent.in_transit);
        assert!(agent.transit_target.is_none());
        fixture.project(arrive(36, destination));
        fixture.counts(destination, if destination == "empfang" { 2 } else { 1 }, 0);
        assert_eq!(fixture.worker.process_pending_batch().unwrap(), 0);
    }
}

#[test]
fn duplicate_event_and_unmirrored_arrival_replay_preserve_views_and_history() {
    let mut fixture = Fixture::new();
    fixture.project(spawn(36, "empfang"));
    fixture.project(start(36, "empfang", "kueche"));
    let offset = fixture.events.get_offset(PROJECTION).unwrap();
    let (arrival_row, arrival) = fixture.append(arrive(36, "kueche"));
    let event_connection = Connection::open(&fixture.event_path).unwrap();
    // Fail only the mirror, after the real worker has committed its view transaction.
    event_connection
        .execute_batch(
            "CREATE TRIGGER fixture_mirror_failure BEFORE INSERT ON projection_offsets
             WHEN NEW.projection_name='sentinel-projection'
             BEGIN SELECT RAISE(ABORT,'fixture offset mirror failure'); END;",
        )
        .unwrap();
    let error = fixture.worker.process_pending_batch().unwrap_err();
    assert!(format!("{error:#}").contains("fixture offset mirror failure"));
    assert_eq!(fixture.events.get_offset(PROJECTION).unwrap(), offset);
    assert_eq!(watermark(&fixture.projection_path), arrival_row);
    fixture.counts("empfang", 0, 0);
    fixture.counts("kueche", 1, 0);
    let views = stable_views(&fixture.projection_path);
    let history = fixture.history();
    event_connection
        .execute_batch("DROP TRIGGER fixture_mirror_failure")
        .unwrap();
    fixture = fixture.reopen();
    assert_eq!(fixture.events.get_offset(PROJECTION).unwrap(), offset);
    assert_eq!(watermark(&fixture.projection_path), arrival_row);
    assert_eq!(fixture.worker.process_pending_batch().unwrap(), 0);
    assert_eq!(
        fixture.events.get_offset(PROJECTION).unwrap(),
        Some(arrival_row)
    );
    assert_eq!(stable_views(&fixture.projection_path), views);
    assert_eq!(fixture.history(), history);

    fixture
        .events
        .legacy_append_gateway(LegacyEventProducer::TestHarness)
        .append_event(&arrival)
        .unwrap();
    assert_eq!(fixture.worker.process_pending_batch().unwrap(), 0);
    assert_eq!(stable_views(&fixture.projection_path), views);
    assert_eq!(fixture.history(), history);
}

#[test]
fn despawn_during_transit_removes_only_active_presence() {
    let mut fixture = Fixture::new();
    for agent in [36, 39] {
        fixture.project(spawn(agent, "empfang"));
        fixture.project(start(agent, "empfang", "kueche"));
    }
    fixture.counts("empfang", 0, 0);
    fixture.counts("kueche", 0, 2);
    fixture.project(despawn(36));
    fixture.counts("empfang", 0, 0);
    fixture.counts("kueche", 0, 1);
    assert_eq!(fixture.agent(36).status, "despawned");
    fixture.project(despawn(36));
    fixture.counts("kueche", 0, 1);
    fixture.project(arrive(36, "kueche"));
    fixture.counts("kueche", 0, 1);
    fixture.project(arrive(39, "kueche"));
    fixture.counts("kueche", 1, 0);
}

#[test]
fn shift_removal_of_multiple_roommates_and_transit_agents_is_exact() {
    let mut fixture = Fixture::new();
    for agent in [36, 37, 38, 39, 40] {
        fixture.project(spawn(agent, "empfang"));
    }
    fixture.project(start(40, "empfang", "kueche"));
    fixture.counts("empfang", 4, 0);
    fixture.counts("kueche", 0, 1);
    let removed = DomainEventPayload::ShiftTransitionCompleted {
        new_shift_set: 2,
        removed_count: 4,
        removed_agents: vec![AgentId(36), AgentId(37), AgentId(38), AgentId(40)],
    };
    let row = fixture.project(removed.clone());
    fixture.counts("empfang", 1, 0);
    fixture.counts("kueche", 0, 0);
    for agent in [36, 37, 38, 40] {
        assert_eq!(fixture.agent(agent).status, "despawned");
        assert_eq!(fixture.agent(agent).last_event_id, row);
    }
    assert_eq!(fixture.agent(39).status, "active");
    fixture.project(removed);
    fixture.counts("empfang", 1, 0);
    fixture.counts("kueche", 0, 0);
}

#[test]
fn inactive_projected_agents_never_contribute_to_either_room_count() {
    for inactive in ["paused", "stopped", "despawned"] {
        let mut fixture = Fixture::new();
        fixture.project(spawn(36, "empfang"));
        fixture.project(status(36, "active", inactive));
        fixture.counts("empfang", 0, 0);
        assert_eq!(fixture.agent(36).current_room.as_deref(), Some("empfang"));
        fixture.project(status(36, inactive, "active"));
        fixture.counts("empfang", 1, 0);
        fixture.project(start(36, "empfang", "kueche"));
        fixture.counts("kueche", 0, 1);
        fixture.project(status(36, "active", inactive));
        fixture.counts("empfang", 0, 0);
        fixture.counts("kueche", 0, 0);
        fixture.project(arrive(36, "kueche"));
        fixture.counts("kueche", 0, 0);
        fixture.project(status(36, inactive, "active"));
        fixture.counts("kueche", 1, 0);
    }
}

#[test]
fn reopen_repairs_only_derived_room_counters_without_resetting_presence_or_progress() {
    let mut fixture = Fixture::new();
    let first = fixture.project(spawn(36, "empfang"));
    fixture.project(spawn(39, "kueche"));
    fixture.project(spawn(40, "empfang"));
    fixture.project(start(36, "empfang", "buero-dev-1"));
    fixture.project(start(40, "empfang", "buero-dev-1"));
    fixture.project(status(40, "active", "paused"));
    fixture.project(DomainEventPayload::RoomPhysicsUpdated {
        room_id: "kueche".into(),
        temperature: 19.25,
        co2_ppm: 713.0,
        noise_db: 37.0,
        occupant_count: 999,
    });
    fixture.project(DomainEventPayload::ChaosTriggered {
        event_type: EventType::PrinterBroken,
        target_room: Some("kueche".into()),
        description: "Synthetic room metadata".into(),
        duration_ticks: 0,
    });
    fixture.project(DomainEventPayload::SmellEventTriggered {
        room_id: "kueche".into(),
        smell_type: "fixture-coffee".into(),
        intensity: 0.3,
        duration_ticks: 10_000,
    });
    fixture.counts("empfang", 0, 0);
    fixture.counts("kueche", 1, 0);
    fixture.counts("buero-dev-1", 0, 1);
    assert_eq!(fixture.worker.catch_up_hierarchy().unwrap(), 9);
    fixture
        .events
        .update_offset("fixture-independent-consumer", first)
        .unwrap();
    let history = fixture.history();
    let offsets = fixture.events.get_all_offsets().unwrap();
    let agents_sql = "SELECT * FROM agent_live_view ORDER BY agent_id";
    let progress_sql =
        "SELECT projection_name,last_event_id FROM projection_watermarks ORDER BY projection_name";
    let agents = rows(&fixture.projection_path, agents_sql);
    let progress = rows(&fixture.projection_path, progress_sql);
    let kpis = rows(
        &fixture.projection_path,
        "SELECT * FROM kpi_1m ORDER BY bucket_start",
    );
    let metadata_sql = "SELECT room_id,active_chaos,active_smells,temperature,co2_ppm,noise_db,
                               last_event_tick,last_event_id,updated_at
                        FROM room_live_view ORDER BY room_id";
    let metadata = rows(&fixture.projection_path, metadata_sql);
    let physics = fixture
        .worker
        .read_store()
        .get_room("kueche")
        .unwrap()
        .unwrap();
    assert_eq!(physics.temperature, Some(19.25));
    assert_eq!(physics.co2_ppm, Some(713.0));
    assert_eq!(physics.noise_db, Some(37.0));
    assert!(physics.active_chaos.is_some());
    assert!(physics.active_smells.is_some());
    // Fixture-only corruption of derived values; source rows and watermarks stay intact.
    Connection::open(&fixture.projection_path)
        .unwrap()
        .execute(
            "UPDATE room_live_view SET occupant_count=43,transit_count=4959",
            [],
        )
        .unwrap();
    fixture.counts("kueche", 43, 4959);
    fixture = fixture.reopen();
    fixture.assert_derived_counts();
    fixture.counts("empfang", 0, 0);
    fixture.counts("kueche", 1, 0);
    fixture.counts("buero-dev-1", 0, 1);
    assert!(fixture.agent(36).in_transit);
    assert_eq!(
        fixture.agent(36).transit_target.as_deref(),
        Some("buero-dev-1")
    );
    assert!(fixture.agent(40).in_transit);
    assert_eq!(fixture.agent(40).status, "paused");
    assert_eq!(rows(&fixture.projection_path, agents_sql), agents);
    assert_eq!(rows(&fixture.projection_path, progress_sql), progress);
    assert_eq!(
        rows(
            &fixture.projection_path,
            "SELECT * FROM kpi_1m ORDER BY bucket_start"
        ),
        kpis,
    );
    assert_eq!(rows(&fixture.projection_path, metadata_sql), metadata);
    assert_eq!(fixture.events.get_all_offsets().unwrap(), offsets);
    assert_eq!(fixture.history(), history);
    assert_eq!(fixture.worker.process_pending_batch().unwrap(), 0);
    assert_eq!(fixture.events.get_all_offsets().unwrap(), offsets);
    assert_eq!(fixture.history(), history);
    fixture.project(arrive(36, "buero-dev-1"));
    fixture.counts("buero-dev-1", 1, 0);
    let after = fixture
        .worker
        .read_store()
        .get_room("kueche")
        .unwrap()
        .unwrap();
    assert_eq!(after.temperature, physics.temperature);
    assert_eq!(after.co2_ppm, physics.co2_ppm);
    assert_eq!(after.noise_db, physics.noise_db);
    assert_eq!(after.active_chaos, physics.active_chaos);
    assert_eq!(after.active_smells, physics.active_smells);
}

#[test]
fn room_counter_failure_rolls_back_spawn_presence_and_watermark_atomically() {
    let mut fixture = Fixture::new();
    let before = stable_views(&fixture.projection_path);
    let (row, _) = fixture.append(spawn(36, "empfang"));
    let history = fixture.history();
    let connection = Connection::open(&fixture.projection_path).unwrap();
    connection
        .execute_batch(
            "CREATE TRIGGER fixture_presence_failure BEFORE UPDATE OF occupant_count,transit_count
             ON room_live_view WHEN NEW.room_id='empfang'
             BEGIN SELECT RAISE(ABORT,'fixture presence failure'); END;",
        )
        .unwrap();
    let error = fixture.worker.process_pending_batch().unwrap_err();
    assert!(format!("{error:#}").contains("fixture presence failure"));
    assert!(fixture.worker.read_store().get_agent(36).unwrap().is_none());
    assert_eq!(stable_views(&fixture.projection_path), before);
    assert_eq!(watermark(&fixture.projection_path), 0);
    assert_eq!(fixture.events.get_offset(PROJECTION).unwrap(), None);
    assert_eq!(fixture.history(), history);
    connection
        .execute_batch("DROP TRIGGER fixture_presence_failure")
        .unwrap();
    assert_eq!(fixture.worker.process_pending_batch().unwrap(), 1);
    assert_eq!(watermark(&fixture.projection_path), row);
    assert_eq!(fixture.events.get_offset(PROJECTION).unwrap(), Some(row));
    fixture.counts("empfang", 1, 0);
    assert_eq!(fixture.agent(36).current_room.as_deref(), Some("empfang"));
    assert_eq!(fixture.history(), history);
}
