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

  it("returns the max last_event_id across agent and room live views", () => {
    projDb.run("INSERT INTO agent_live_view (last_event_id) VALUES (100)");
    projDb.run("INSERT INTO room_live_view (last_event_id) VALUES (150)");

    expect(getGlobalMaxEventId()).toBe(150);
  });

  it("returns zero when both live views are empty", () => {
    expect(getGlobalMaxEventId()).toBe(0);
  });
});
