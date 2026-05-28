import { afterEach, beforeEach, describe, expect, it } from "bun:test";
import type { ServerWebSocket } from "bun";
import { Database } from "bun:sqlite";
import { closeDatabases, setDatabases } from "../db";
import { createWsHandler, pollForChanges, resetWatermarks } from "../ws";

let projDb: Database;
let esDb: Database;
let sentMessages: Array<Record<string, unknown>>;
let fakeWs: ServerWebSocket<unknown>;

function setupDatabases(): void {
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
      hunger REAL DEFAULT 0,
      energy REAL DEFAULT 1,
      stress REAL DEFAULT 0,
      bladder REAL DEFAULT 0,
      social_need REAL DEFAULT 0,
      caffeine_mg REAL DEFAULT 0,
      mood TEXT,
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
      temperature REAL,
      co2_ppm REAL,
      noise_db REAL,
      last_event_tick INTEGER,
      last_event_id INTEGER NOT NULL DEFAULT 0,
      updated_at INTEGER NOT NULL
    )
  `);
  projDb.run(`
    INSERT INTO agent_live_view
      (agent_id, name, role, shift_set, status, current_room, in_transit, transit_target, last_action, last_action_tick,
       hunger, energy, stress, bladder, social_need, caffeine_mg, mood, last_event_id, updated_at)
    VALUES
      (1, 'Anna Schmidt', 'developer', 1, 'active', 'kueche', 0, NULL, 'Kaffee holen', 118,
       20, 70, 15, 5, 30, 80, 'Focused', 100, 1000)
  `);
  projDb.run(`
    INSERT INTO room_live_view
      (room_id, occupant_count, transit_count, active_chaos, active_smells, temperature, co2_ppm, noise_db, last_event_tick, last_event_id, updated_at)
    VALUES
      ('kueche', 1, 0, NULL, NULL, 22.5, 520, 41, 120, 150, 1000)
  `);
  projDb.run(`
    CREATE TABLE kpi_1m (
      bucket_start INTEGER PRIMARY KEY,
      last_event_id INTEGER NOT NULL DEFAULT 0
    )
  `);
  projDb.run("INSERT INTO kpi_1m (bucket_start, last_event_id) VALUES (1000, 140)");
  projDb.run(`
    CREATE TABLE projection_watermarks (
      projection_name TEXT PRIMARY KEY,
      last_event_id INTEGER NOT NULL DEFAULT 0,
      updated_at INTEGER NOT NULL
    )
  `);
  projDb
    .prepare("INSERT INTO projection_watermarks VALUES (?, ?, ?)")
    .run("sentinel-projection", 150, Date.now());

  esDb = new Database(":memory:");
  setDatabases(projDb, esDb);
}

function openFakeClient(): void {
  sentMessages = [];
  fakeWs = {
    send(message: string) {
      sentMessages.push(JSON.parse(message) as Record<string, unknown>);
    },
  } as unknown as ServerWebSocket<unknown>;
  createWsHandler().open(fakeWs);
}

function messageTypes(): unknown[] {
  return sentMessages.map((message) => message.type);
}

describe("WebSocket global polling (#277)", () => {
  beforeEach(() => {
    setupDatabases();
    resetWatermarks();
    openFakeClient();
  });

  afterEach(() => {
    createWsHandler().close(fakeWs);
    closeDatabases();
    resetWatermarks();
  });

  it("does not rebroadcast when the global watermark is unchanged", () => {
    pollForChanges();
    pollForChanges();

    expect(messageTypes()).toEqual([
      "agent_update",
      "room_update",
      "cockpit_update",
      "chaos_update",
      "activity_update",
    ]);
  });

  it("skips DB polling when no WebSocket clients are connected", () => {
    createWsHandler().close(fakeWs);
    sentMessages = [];
    projDb.run("DROP TABLE projection_watermarks");
    projDb.run("DROP TABLE agent_live_view");
    projDb.run("DROP TABLE room_live_view");
    projDb.run("DROP TABLE kpi_1m");

    expect(() => pollForChanges()).not.toThrow();
    expect(messageTypes()).toEqual([]);
  });

  it("broadcasts all view update types when the global watermark changes", () => {
    pollForChanges();
    projDb.run("UPDATE projection_watermarks SET last_event_id = 151 WHERE projection_name = 'sentinel-projection'");

    pollForChanges();

    expect(messageTypes()).toEqual([
      "agent_update",
      "room_update",
      "cockpit_update",
      "chaos_update",
      "activity_update",
      "agent_update",
      "room_update",
      "cockpit_update",
      "chaos_update",
      "activity_update",
    ]);
    expect((sentMessages[5].agents as unknown[]).length).toBe(1);
    expect((sentMessages[6].rooms as unknown[]).length).toBe(1);
  });

  it("resetWatermarks forces the next poll to broadcast a full update again", () => {
    pollForChanges();
    resetWatermarks();

    pollForChanges();

    expect(messageTypes()).toEqual([
      "agent_update",
      "room_update",
      "cockpit_update",
      "chaos_update",
      "activity_update",
      "agent_update",
      "room_update",
      "cockpit_update",
      "chaos_update",
      "activity_update",
    ]);
  });
});
