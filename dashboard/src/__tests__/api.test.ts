import { describe, expect, it, beforeAll, afterAll } from "bun:test";
import { Database } from "bun:sqlite";
import { setDatabases, closeDatabases } from "../db";
import { ALL_ROOM_IDS } from "../rooms-meta";
import { app } from "../index";

let projDb: Database;
let esDb: Database;

beforeAll(() => {
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

  // 1 Agent
  projDb.run(
    "INSERT INTO agent_live_view VALUES (1,'Test Agent','tester',0,'active','buero-dev-1',0,null,'Testing',10,20,1000)",
  );

  // 15 Rooms aus ROOM_METADATA
  const insertRoom = projDb.prepare(
    "INSERT INTO room_live_view VALUES (?,0,0,null,null,null,0,1000)",
  );
  for (const id of ALL_ROOM_IDS) {
    insertRoom.run(id);
  }

  // 1 KPI
  projDb.run("INSERT INTO kpi_1m VALUES (1000,1,10,5,0,100,0,0,20,1000)");

  // EventStore
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
  esDb.run("INSERT INTO events VALUES (1,'e1','AgentActed','a1','{}','c1',null,'o1',1,1000)");
  esDb.run("INSERT INTO projection_offsets VALUES ('sentinel-projection',1,1000)");

  setDatabases(projDb, esDb);
});

afterAll(() => {
  closeDatabases();
});

describe("Dashboard API", () => {
  it("GET /api/health returns ok with projection_lag", async () => {
    const res = await app.request("/api/health");
    expect(res.status).toBe(200);
    const data = await res.json();
    expect(data.status).toBe("ok");
    expect(typeof data.uptime).toBe("number");
    expect(data.projection_lag).toBe(0); // 1 event, offset 1
  });

  it("GET /api/agents returns agent list from DB", async () => {
    const res = await app.request("/api/agents");
    expect(res.status).toBe(200);
    const data = await res.json();
    expect(Array.isArray(data)).toBe(true);
    expect(data.length).toBe(1);
    expect(data[0].name).toBe("Test Agent");
    expect(data[0].role).toBe("tester");
    expect(data[0].in_transit).toBe(false);
  });

  it("GET /api/rooms returns 26 rooms from DB", async () => {
    const res = await app.request("/api/rooms");
    expect(res.status).toBe(200);
    const data = await res.json();
    expect(Array.isArray(data)).toBe(true);
    expect(data.length).toBe(26);
    // Rooms haben Metadata aus rooms-meta.ts
    const buero = data.find((r: { id: string }) => r.id === "buero-dev-1");
    expect(buero).toBeDefined();
    expect(buero.name).toBe("Entwicklungsbüro 1");
    expect(buero.floor).toBe(0);
  });

  it("GET /api/metrics returns KPI from DB", async () => {
    const res = await app.request("/api/metrics");
    expect(res.status).toBe(200);
    const data = await res.json();
    expect(data.active_agents).toBe(1);
    expect(data.total_actions).toBe(10);
    expect(data.total_transits).toBe(5);
  });

  it("GET /api/metrics/benchmarks returns issue #276 VM-relative results", async () => {
    const res = await app.request("/api/metrics/benchmarks");
    expect(res.status).toBe(200);
    const data = await res.json();
    expect(data.issue).toBe(276);
    expect(data.cpu).toContain("i7-3930K");
    expect(data.comparison_scope).toContain("Deploy-VM same-machine");
    expect(data.results.map((row: { id: string }) => row.id)).toEqual([
      "physics",
      "perception",
      "persist",
      "persist-prebuilt",
      "bio-tick",
    ]);
    expect(data.results[0].before_ns_per_iter).toBe(1_172_325);
    expect(data.results[0].after_ns_per_iter).toBe(857_440);
  });

  it("GET /api/agents/:id/state returns 404 for unknown", async () => {
    const res = await app.request("/api/agents/999/state");
    expect(res.status).toBe(404);
  });
});
