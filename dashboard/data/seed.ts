// Erstellt Test-Datenbanken fuer lokales Dashboard-Development.
// Usage: bun run data/seed.ts

import { Database } from "bun:sqlite";

const projDb = new Database("data/projection.db");
const esDb = new Database("data/events.db");

// Projection DB Schemas
projDb.run(`
  CREATE TABLE IF NOT EXISTS agent_live_view (
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
  CREATE TABLE IF NOT EXISTS room_live_view (
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
  CREATE TABLE IF NOT EXISTS kpi_1m (
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

// EventStore DB Schemas
esDb.run(`
  CREATE TABLE IF NOT EXISTS events (
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
  CREATE TABLE IF NOT EXISTS projection_offsets (
    projection_name TEXT PRIMARY KEY,
    last_event_id INTEGER NOT NULL DEFAULT 0,
    updated_at INTEGER NOT NULL
  )
`);

// ── Seed Agents ─────────────────────────────────
const now = Date.now();

const agents = [
  [1, "Thomas Müller", "Abteilungsleiter", 0, "active", "buero-ceo", 0, null, "Quartalsbericht gelesen", 142, 200, now],
  [2, "Anna Schmidt", "Entwicklerin", 0, "active", "buero-dev-1", 0, null, "Feature implementiert", 140, 198, now],
  [3, "Max Weber", "Designer", 0, "active", "kueche", 0, null, "Kaffee geholt", 139, 195, now],
  [4, "Lisa Baumann", "QA Ingenieurin", 0, "active", "buero-dev-2", 0, null, "Tests ausgeführt", 138, 193, now],
  [5, "Markus Braun", "DevOps", 1, "active", null, 1, "buero-dev-1", null, null, 190, now],
  [6, "Sarah Koch", "Produktmanagerin", 0, "active", "meetingraum-01", 0, null, "Sprint geplant", 135, 188, now],
  [7, "Felix Richter", "Praktikant", 0, "active", "buero-design-1", 0, null, "Mockups erstellt", 130, 185, now],
  [8, "Mia Fischer", "Entwicklerin", 0, "active", "buero-dev-1", 0, null, "Code reviewed", 128, 183, now],
];
const insertAgent = projDb.prepare(
  "INSERT OR REPLACE INTO agent_live_view VALUES (?,?,?,?,?,?,?,?,?,?,?,?)",
);
for (const a of agents) insertAgent.run(...a);

// ── Seed Rooms ──────────────────────────────────
const roomIds = [
  "empfang", "flur-eg", "kueche", "buero-dev-1", "buero-dev-2",
  "meetingraum-01", "toilette-eg-damen", "toilette-eg-herren", "treppenhaus", "flur-og",
  "buero-design-1", "buero-design-2", "buero-ceo",
  "meetingraum-02", "meetingraum-03", "toilette-og-damen", "toilette-og-herren",
];

const insertRoom = projDb.prepare(
  "INSERT OR REPLACE INTO room_live_view VALUES (?,?,?,?,?,?,?)",
);
for (const id of roomIds) {
  let occ = 0;
  if (id === "buero-dev-1") occ = 3;
  else if (id === "buero-ceo") occ = 1;
  else if (id === "kueche") occ = 1;
  else if (id === "meetingraum-01") occ = 1;
  else if (id === "buero-dev-2") occ = 1;
  else if (id === "buero-design-1") occ = 1;

  const transit = id === "buero-dev-1" ? 1 : 0;
  const chaos = id === "treppenhaus" ? '{"type":"fire_alarm","severity":"medium","tick":120}' : null;
  insertRoom.run(id, occ, transit, chaos, 142, 200, now);
}

// ── Seed KPI ────────────────────────────────────
projDb.run(
  `INSERT OR REPLACE INTO kpi_1m VALUES (${Math.floor(now / 60000) * 60000}, 8, 347, 89, 3, 1420, 2, 0, 200, ${now})`,
);

// ── Seed Events + Offset ────────────────────────
const insertEvent = esDb.prepare(
  "INSERT OR REPLACE INTO events VALUES (?,?,?,?,?,?,?,?,?,?)",
);
for (let i = 1; i <= 210; i++) {
  insertEvent.run(
    i, `evt-${i}`, "AgentActed", `agent-${i % 8}`, "{}", `corr-${i}`, null, `op-${i}`, i, now,
  );
}
esDb.prepare(
  "INSERT OR REPLACE INTO projection_offsets VALUES (?,?,?)",
).run("sentinel-projection", 200, now);

projDb.close();
esDb.close();

console.log("Seed complete: data/projection.db + data/events.db");
console.log("  8 agents, 15 rooms, 1 KPI bucket, 210 events (lag=10)");
