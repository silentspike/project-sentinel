import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import { Database } from "bun:sqlite";
import { app } from "../index";
import { setDatabases } from "../db";

describe("Events Routes", () => {
  let projDb: Database;
  let esDb: Database;

  beforeAll(() => {
    // In-memory DBs fuer Tests
    projDb = new Database(":memory:");
    esDb = new Database(":memory:");

    // Minimales Schema fuer EventStore
    esDb.run(`CREATE TABLE events (
      id INTEGER PRIMARY KEY AUTOINCREMENT,
      event_id TEXT NOT NULL,
      event_type TEXT NOT NULL,
      aggregate_id TEXT NOT NULL,
      payload TEXT NOT NULL DEFAULT '{}',
      correlation_id TEXT NOT NULL DEFAULT '',
      causation_id TEXT,
      tick INTEGER NOT NULL DEFAULT 0,
      timestamp_ms INTEGER NOT NULL,
      compensation_type TEXT NOT NULL DEFAULT 'none'
    )`);

    // Minimale Projection-Tabellen
    projDb.run(`CREATE TABLE agent_live_view (
      agent_id INTEGER PRIMARY KEY,
      name TEXT, role TEXT, shift_set INTEGER DEFAULT 1,
      status TEXT DEFAULT 'active', current_room TEXT,
      in_transit INTEGER DEFAULT 0, transit_target TEXT,
      last_action TEXT, last_action_tick INTEGER,
      hunger REAL DEFAULT 0, energy REAL DEFAULT 1,
      stress REAL DEFAULT 0, bladder REAL DEFAULT 0,
      social_need REAL DEFAULT 0, caffeine_mg REAL DEFAULT 0,
      mood TEXT, last_event_id INTEGER DEFAULT 0, updated_at INTEGER DEFAULT 0
    )`);

    projDb.run(`CREATE TABLE room_live_view (
      room_id TEXT PRIMARY KEY,
      occupant_count INTEGER DEFAULT 0, transit_count INTEGER DEFAULT 0,
      active_chaos TEXT, active_smells TEXT, temperature REAL, co2_ppm REAL, noise_db REAL,
      last_event_tick INTEGER, last_event_id INTEGER DEFAULT 0, updated_at INTEGER DEFAULT 0
    )`);

    projDb.run(`CREATE TABLE kpi_1m (
      bucket_start INTEGER PRIMARY KEY,
      active_agents INTEGER DEFAULT 0, total_actions INTEGER DEFAULT 0,
      total_transits INTEGER DEFAULT 0, chaos_events INTEGER DEFAULT 0,
      tick_count INTEGER DEFAULT 0, shift_changes INTEGER DEFAULT 0,
      nightrun_events INTEGER DEFAULT 0, last_event_id INTEGER DEFAULT 0,
      updated_at INTEGER DEFAULT 0
    )`);

    projDb.run(`CREATE TABLE projection_offsets (
      projection_name TEXT PRIMARY KEY,
      last_event_id INTEGER DEFAULT 0
    )`);

    // Test-Events einfuegen
    const now = Date.now();
    const insert = esDb.prepare(
      `INSERT INTO events (event_id, event_type, aggregate_id, payload, tick, timestamp_ms)
       VALUES (?, ?, ?, ?, ?, ?)`
    );

    insert.run("evt-1", "chaos_triggered", "buero-dev-1", '{"event_type":"PhoneRing"}', 100, now - 60000);
    insert.run("evt-2", "agent_action_received", "AGENT-01", '{"action_type":"speak"}', 101, now - 50000);
    insert.run("evt-3", "chaos_triggered", "kueche-eg", '{"event_type":"PrinterBroken"}', 102, now - 40000);
    insert.run("evt-4", "agent_spawned", "AGENT-02", '{}', 103, now - 30000);
    insert.run("evt-5", "bio_action_performed", "AGENT-01", '{}', 104, now - 20000);

    setDatabases(projDb, esDb);
  });

  afterAll(() => {
    projDb.close();
    esDb.close();
  });

  describe("GET /api/events", () => {
    it("returns all events with default limit", async () => {
      const res = await app.request("/api/events");
      expect(res.status).toBe(200);
      const body = await res.json();
      expect(body.events.length).toBe(5);
      expect(body.total).toBe(5);
      // Newest first (ORDER BY id DESC)
      expect(body.events[0].event_id).toBe("evt-5");
    });

    it("filters by event type", async () => {
      const res = await app.request("/api/events?type=chaos_triggered");
      expect(res.status).toBe(200);
      const body = await res.json();
      expect(body.events.length).toBe(2);
      expect(body.total).toBe(2);
      for (const e of body.events) {
        expect(e.event_type).toBe("chaos_triggered");
      }
    });

    it("filters by agent", async () => {
      const res = await app.request("/api/events?agent=AGENT-01");
      expect(res.status).toBe(200);
      const body = await res.json();
      expect(body.events.length).toBe(2);
      for (const e of body.events) {
        expect(e.aggregate_id).toBe("AGENT-01");
      }
    });

    it("combines type and agent filters", async () => {
      const res = await app.request("/api/events?type=agent_action_received&agent=AGENT-01");
      expect(res.status).toBe(200);
      const body = await res.json();
      expect(body.events.length).toBe(1);
      expect(body.events[0].event_id).toBe("evt-2");
    });

    it("respects limit parameter", async () => {
      const res = await app.request("/api/events?limit=2");
      expect(res.status).toBe(200);
      const body = await res.json();
      expect(body.events.length).toBe(2);
      expect(body.total).toBe(5);
      expect(body.limit).toBe(2);
    });

    it("respects offset parameter", async () => {
      const res = await app.request("/api/events?limit=2&offset=3");
      expect(res.status).toBe(200);
      const body = await res.json();
      expect(body.events.length).toBe(2);
      expect(body.offset).toBe(3);
    });
  });

  describe("GET /api/events/types", () => {
    it("returns event type counts", async () => {
      const res = await app.request("/api/events/types");
      expect(res.status).toBe(200);
      const body = await res.json();
      expect(body.types.length).toBeGreaterThan(0);

      const chaosType = body.types.find((t: any) => t.event_type === "chaos_triggered");
      expect(chaosType).toBeTruthy();
      expect(chaosType.cnt).toBe(2);
    });
  });
});
