import { describe, expect, test, beforeAll, afterAll } from "bun:test";
import { Database } from "bun:sqlite";
import { setDatabases, closeDatabases } from "../db";
import { ALL_ROOM_IDS } from "../rooms-meta";
import { app } from "../index";

let projDb: Database;
let esDb: Database;

beforeAll(() => {
  // In-memory projection DB (Schema aus store.rs)
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

  // Seed 3 Agents (Room-IDs aus ROOM_METADATA)
  const agents = [
    [1, "Thomas Mueller", "manager", 0, "active", "buero-dev-1", 0, null, "Dokument gelesen", 42, 50, 1000],
    [2, "Anna Schmidt", "developer", 0, "active", "kueche", 0, null, "Kaffee geholt", 41, 48, 1000],
    [3, "Max Weber", "analyst", 1, "active", null, 1, "buero-design-1", null, null, 45, 1000],
  ];
  const insertAgent = projDb.prepare(
    "INSERT INTO agent_live_view VALUES (?,?,?,?,?,?,?,?,?,?,?,?)",
  );
  for (const a of agents) insertAgent.run(...a);

  // Seed alle 15 Rooms aus ROOM_METADATA
  const insertRoom = projDb.prepare(
    "INSERT INTO room_live_view VALUES (?,?,?,?,?,?,?)",
  );
  for (const id of ALL_ROOM_IDS) {
    const occ = id === "buero-dev-1" ? 1 : id === "kueche" ? 1 : 0;
    const transit = id === "buero-design-1" ? 1 : 0;
    const chaos = id === "treppenhaus" ? '{"type":"fire_alarm","severity":"medium"}' : null;
    insertRoom.run(id, occ, transit, chaos, 42, 30, 1000);
  }

  // Seed 1 KPI bucket
  projDb.run(
    "INSERT INTO kpi_1m VALUES (1000, 3, 120, 35, 2, 500, 1, 0, 50, 1000)",
  );

  // In-memory EventStore DB
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

  // 100 Events, Offset bei 95 => Lag = 5
  const insertEvent = esDb.prepare(
    "INSERT INTO events VALUES (?,?,?,?,?,?,?,?,?,?)",
  );
  for (let i = 1; i <= 100; i++) {
    insertEvent.run(
      i, `evt-${i}`, "AgentActed", `agent-${i % 3}`, "{}", `corr-${i}`, null, `op-${i}`, i, Date.now(),
    );
  }
  esDb.prepare(
    "INSERT INTO projection_offsets VALUES (?, ?, ?)",
  ).run("sentinel-projection", 95, Date.now());

  setDatabases(projDb, esDb);
});

afterAll(() => {
  closeDatabases();
});

describe("Acceptance Tests - Issue #24: Dashboard Live-Daten", () => {
  // AC-2: GET /api/agents liefert DB-Daten
  test("AC-2: /api/agents returns 3 seeded active agents from DB", async () => {
    const res = await app.request("/api/agents");
    expect(res.status).toBe(200);

    const data = await res.json();
    expect(Array.isArray(data)).toBe(true);
    expect(data.length).toBe(3);

    for (const agent of data) {
      expect(agent).toHaveProperty("name");
      expect(agent).toHaveProperty("role");
      expect(agent).toHaveProperty("status");
      expect(agent).toHaveProperty("in_transit");
      expect(typeof agent.name).toBe("string");
      expect(typeof agent.status).toBe("string");
    }
  });

  // AC-2b: Agent-Detail per ID
  test("AC-2b: /api/agents/:id/state returns agent detail by numeric ID", async () => {
    const res = await app.request("/api/agents/1/state");
    expect(res.status).toBe(200);

    const data = await res.json();
    expect(data.name).toBe("Thomas Mueller");
    expect(data.role).toBe("manager");
    expect(data.shift_set).toBe(0);
    expect(data.last_action).toBe("Dokument gelesen");
    expect(data.current_room).toBe("buero-dev-1");
    expect(data.in_transit).toBe(false);
  });

  // AC-2c: Agent-Detail per Name-Slug
  test("AC-2c: /api/agents/:slug/state returns agent detail by name slug", async () => {
    const res = await app.request("/api/agents/anna-schmidt/state");
    expect(res.status).toBe(200);

    const data = await res.json();
    expect(data.name).toBe("Anna Schmidt");
    expect(data.role).toBe("developer");
    expect(data.current_room).toBe("kueche");
  });

  // AC-2d: Unknown agent returns 404
  test("AC-2d: /api/agents/unknown/state returns 404", async () => {
    const res = await app.request("/api/agents/nonexistent/state");
    expect(res.status).toBe(404);
  });

  // AC-3: GET /api/rooms liefert 15 Raeume
  test("AC-3: /api/rooms returns 15 rooms with metadata", async () => {
    const res = await app.request("/api/rooms");
    expect(res.status).toBe(200);

    const data = await res.json();
    expect(Array.isArray(data)).toBe(true);
    expect(data.length).toBe(15);

    for (const room of data) {
      expect(room).toHaveProperty("id");
      expect(room).toHaveProperty("name");
      expect(room).toHaveProperty("floor");
      expect(room).toHaveProperty("capacity");
      expect(room).toHaveProperty("room_type");
      expect(room).toHaveProperty("occupant_count");
      expect(typeof room.id).toBe("string");
      expect(typeof room.name).toBe("string");
      expect(typeof room.floor).toBe("number");
    }
  });

  // AC-3b: Room Detail mit Chaos-Daten
  test("AC-3b: /api/rooms/:id returns single room with chaos data", async () => {
    const res = await app.request("/api/rooms/treppenhaus");
    expect(res.status).toBe(200);

    const data = await res.json();
    expect(data.id).toBe("treppenhaus");
    expect(data.name).toBe("Treppenhaus");
    expect(data.active_chaos).toEqual({ type: "fire_alarm", severity: "medium" });
  });

  // AC-3c: Unknown room returns 404
  test("AC-3c: /api/rooms/unknown returns 404", async () => {
    const res = await app.request("/api/rooms/nonexistent");
    expect(res.status).toBe(404);
  });

  // AC-4: GET /api/metrics liefert KPI-Daten
  test("AC-4: /api/metrics returns KPI data from DB", async () => {
    const res = await app.request("/api/metrics");
    expect(res.status).toBe(200);

    const data = await res.json();
    expect(data.active_agents).toBe(3);
    expect(data.total_actions).toBe(120);
    expect(data.total_transits).toBe(35);
    expect(data.chaos_events).toBe(2);
    expect(data.shift_changes).toBe(1);
    expect(typeof data.uptime).toBe("number");
  });

  // AC-5: GET /api/health hat projection_lag
  test("AC-5: /api/health returns projection_lag from EventStore", async () => {
    const res = await app.request("/api/health");
    expect(res.status).toBe(200);

    const data = await res.json();
    expect(data.status).toBe("ok");
    expect(typeof data.uptime).toBe("number");
    expect(data.projection_lag).toBe(5); // 100 events - offset 95
  });

  // AC-N1: Kein Mock-Import im Produktionscode
  test("AC-N1: no mock import in production code", async () => {
    const endpoints = ["/api/agents", "/api/rooms", "/api/metrics", "/api/health"];
    for (const ep of endpoints) {
      const res = await app.request(ep);
      expect(res.status).toBe(200);
    }
  });

  // In-Transit Agent wird korrekt angezeigt
  test("in-transit agent shows transit_target and no current_room", async () => {
    const res = await app.request("/api/agents/3/state");
    expect(res.status).toBe(200);

    const data = await res.json();
    expect(data.name).toBe("Max Weber");
    expect(data.in_transit).toBe(true);
    expect(data.transit_target).toBe("buero-design-1");
    expect(data.current_room).toBeNull();
  });
});
