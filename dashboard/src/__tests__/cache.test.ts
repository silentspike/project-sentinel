import { describe, expect, it, beforeEach, afterEach } from "bun:test";
import { Database } from "bun:sqlite";
import { setDatabases, closeDatabases, getAgentNameMap, resetCaches } from "../db";
import { ALL_ROOM_IDS } from "../rooms-meta";

let projDb: Database;
let esDb: Database;

function setupDatabases() {
  projDb = new Database(":memory:");
  projDb.run(`
    CREATE TABLE agent_live_view (
      agent_id INTEGER PRIMARY KEY,
      name TEXT NOT NULL,
      role TEXT NOT NULL,
      shift_set INTEGER NOT NULL,
      status TEXT NOT NULL DEFAULT 'active',
      current_room TEXT,
      in_transit INTEGER NOT NULL DEFAULT 0,
      transit_target TEXT,
      last_action TEXT,
      last_action_tick INTEGER,
      last_event_id INTEGER NOT NULL DEFAULT 0,
      updated_at INTEGER NOT NULL
    )
  `);
  projDb.run(`
    CREATE TABLE room_live_view (
      room_id TEXT PRIMARY KEY,
      occupant_count INTEGER NOT NULL DEFAULT 0,
      transit_count INTEGER NOT NULL DEFAULT 0,
      active_chaos TEXT,
      active_smells TEXT,
      last_event_tick INTEGER,
      last_event_id INTEGER NOT NULL DEFAULT 0,
      updated_at INTEGER NOT NULL
    )
  `);
  projDb.run(`
    CREATE TABLE kpi_1m (
      bucket_start INTEGER PRIMARY KEY,
      active_agents INTEGER NOT NULL DEFAULT 0,
      total_actions INTEGER NOT NULL DEFAULT 0,
      total_transits INTEGER NOT NULL DEFAULT 0,
      chaos_events INTEGER NOT NULL DEFAULT 0,
      tick_count INTEGER NOT NULL DEFAULT 0,
      shift_changes INTEGER NOT NULL DEFAULT 0,
      nightrun_events INTEGER NOT NULL DEFAULT 0,
      last_event_id INTEGER NOT NULL DEFAULT 0,
      updated_at INTEGER NOT NULL
    )
  `);

  const insertRoom = projDb.prepare(
    "INSERT INTO room_live_view VALUES (?,0,0,null,null,null,0,1000)",
  );
  for (const id of ALL_ROOM_IDS) {
    insertRoom.run(id);
  }

  esDb = new Database(":memory:");
  esDb.run(`
    CREATE TABLE events (
      id INTEGER PRIMARY KEY AUTOINCREMENT,
      event_id TEXT NOT NULL UNIQUE,
      event_type TEXT NOT NULL,
      aggregate_id TEXT NOT NULL,
      payload TEXT NOT NULL,
      correlation_id TEXT NOT NULL,
      causation_id TEXT,
      operation_id TEXT NOT NULL,
      tick INTEGER NOT NULL,
      timestamp_ms INTEGER NOT NULL
    )
  `);
  esDb.run(`
    CREATE TABLE projection_offsets (
      projection_name TEXT PRIMARY KEY,
      last_event_id INTEGER NOT NULL DEFAULT 0,
      updated_at INTEGER NOT NULL
    )
  `);
  esDb.run(
    "INSERT INTO events VALUES (1,'e1','AgentActed','a1','{}','c1',null,'o1',1,1000)",
  );
  esDb.run(
    "INSERT INTO projection_offsets VALUES ('sentinel-projection',1,1000)",
  );
}

describe("Agent Name Cache (#253)", () => {
  beforeEach(() => {
    setupDatabases();
    // Agent der alten Schicht
    projDb.run(
      "INSERT INTO agent_live_view VALUES (1,'Sandra Vogel','CEO',3,'active','buero-dev-1',0,null,'Working',10,20,1000)",
    );
    setDatabases(projDb, esDb);
  });

  afterEach(() => {
    closeDatabases();
  });

  it("liefert gecachte Werte bei wiederholtem Aufruf", () => {
    const map1 = getAgentNameMap();
    const map2 = getAgentNameMap();
    // Gleiche Map-Referenz = Cache-Hit
    expect(map1).toBe(map2);
    expect(map1.get(1)).toBe("Sandra Vogel");
  });

  it("invalidiert Cache nach resetCaches()", () => {
    const map1 = getAgentNameMap();
    expect(map1.get(1)).toBe("Sandra Vogel");

    // DB aendern (simuliert Restore/Schichtwechsel)
    projDb.run(
      "UPDATE agent_live_view SET name = 'Michael Hartmann' WHERE agent_id = 1",
    );

    // Ohne Reset: Cache liefert alten Wert
    const map2 = getAgentNameMap();
    expect(map2.get(1)).toBe("Sandra Vogel");

    // Mit Reset: Cache liefert neuen Wert
    resetCaches();
    const map3 = getAgentNameMap();
    expect(map3.get(1)).toBe("Michael Hartmann");
  });
});
