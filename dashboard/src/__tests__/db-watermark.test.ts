import { afterEach, beforeEach, describe, expect, it } from "bun:test";
import { Database } from "bun:sqlite";
import { closeDatabases, getGlobalMaxEventId, setDatabases } from "../db";

let projDb: Database;
let esDb: Database;

function setupProjectionViews(): void {
  projDb = new Database(":memory:");
  projDb.run(`
    CREATE TABLE agent_live_view (
      last_event_id INTEGER NOT NULL DEFAULT 0
    )
  `);
  projDb.run(`
    CREATE TABLE room_live_view (
      last_event_id INTEGER NOT NULL DEFAULT 0
    )
  `);
  projDb.run(`
    CREATE TABLE kpi_1m (
      last_event_id INTEGER NOT NULL DEFAULT 0
    )
  `);
  projDb.run(`
    CREATE TABLE projection_watermarks (
      projection_name TEXT PRIMARY KEY,
      last_event_id INTEGER NOT NULL DEFAULT 0,
      updated_at INTEGER NOT NULL
    )
  `);
  esDb = new Database(":memory:");
  setDatabases(projDb, esDb);
}

describe("global WebSocket watermark (#277)", () => {
  beforeEach(() => {
    setupProjectionViews();
  });

  afterEach(() => {
    closeDatabases();
  });

  it("returns the persisted projection watermark", () => {
    projDb.run("INSERT INTO agent_live_view (last_event_id) VALUES (100)");
    projDb.run("INSERT INTO room_live_view (last_event_id) VALUES (150)");
    projDb.run("INSERT INTO kpi_1m (last_event_id) VALUES (175)");
    projDb
      .prepare("INSERT INTO projection_watermarks VALUES (?, ?, ?)")
      .run("sentinel-projection", 210, Date.now());

    expect(getGlobalMaxEventId()).toBe(210);
  });

  it("falls back to live views and KPI buckets for pre-watermark databases", () => {
    projDb.run("INSERT INTO agent_live_view (last_event_id) VALUES (100)");
    projDb.run("INSERT INTO room_live_view (last_event_id) VALUES (150)");
    projDb.run("INSERT INTO kpi_1m (last_event_id) VALUES (180)");
    projDb.run("DROP TABLE projection_watermarks");

    expect(getGlobalMaxEventId()).toBe(180);
  });

  it("falls back to live views and KPI buckets when the watermark row is absent", () => {
    projDb.run("INSERT INTO agent_live_view (last_event_id) VALUES (100)");
    projDb.run("INSERT INTO room_live_view (last_event_id) VALUES (150)");
    projDb.run("INSERT INTO kpi_1m (last_event_id) VALUES (190)");

    expect(getGlobalMaxEventId()).toBe(190);
  });

  it("returns zero when no projection watermark exists", () => {
    expect(getGlobalMaxEventId()).toBe(0);
  });
});
