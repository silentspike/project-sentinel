import { describe, test, expect, beforeAll, afterAll } from "bun:test";
import { Database } from "bun:sqlite";
import { app } from "../index";
import { setDatabases } from "../db";

// ── In-Memory Test Databases ──────────────────────

let projDb: Database;
let esDb: Database;

const EVENTS_SCHEMA = `
  CREATE TABLE events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    event_id TEXT NOT NULL UNIQUE,
    event_type TEXT NOT NULL,
    aggregate_id TEXT NOT NULL,
    payload TEXT NOT NULL,
    correlation_id TEXT NOT NULL,
    causation_id TEXT,
    operation_id TEXT NOT NULL UNIQUE,
    tick INTEGER NOT NULL,
    timestamp_ms INTEGER NOT NULL,
    schema_version INTEGER DEFAULT 1,
    compensation_type TEXT DEFAULT 'none'
  );
  CREATE INDEX idx_events_type ON events(event_type, id);
  CREATE INDEX idx_events_correlation ON events(correlation_id);
`;

const PROJECTION_SCHEMA = `
  CREATE TABLE agent_live_view (
    agent_id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    role TEXT NOT NULL,
    shift_set INTEGER NOT NULL,
    status TEXT NOT NULL,
    current_room TEXT,
    in_transit INTEGER DEFAULT 0,
    transit_target TEXT,
    last_action TEXT,
    last_action_tick INTEGER,
    last_event_id INTEGER DEFAULT 0,
    updated_at INTEGER DEFAULT 0
  );
  CREATE TABLE room_live_view (
    room_id TEXT PRIMARY KEY,
    occupant_count INTEGER DEFAULT 0,
    transit_count INTEGER DEFAULT 0,
    active_chaos TEXT,
    last_event_tick INTEGER,
    last_event_id INTEGER DEFAULT 0,
    updated_at INTEGER DEFAULT 0
  );
  CREATE TABLE projection_offsets (
    projection_name TEXT PRIMARY KEY,
    last_event_id INTEGER NOT NULL
  );
`;

const EVOLUTION_SCHEMA = `
  CREATE TABLE personality_evolution (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    agent_id TEXT NOT NULL,
    tick INTEGER NOT NULL,
    field TEXT NOT NULL,
    change_type TEXT NOT NULL,
    old_value TEXT,
    new_value TEXT NOT NULL,
    reason TEXT NOT NULL,
    nmda_score REAL,
    source TEXT DEFAULT 'realtime_judge',
    created_at_ms INTEGER NOT NULL
  );
  CREATE INDEX idx_evolution_agent ON personality_evolution(agent_id, tick);
`;

const now = Date.now();

function seedEvents(db: Database): void {
  const insert = db.prepare(`
    INSERT INTO events (event_id, event_type, aggregate_id, payload,
                        correlation_id, causation_id, operation_id,
                        tick, timestamp_ms, compensation_type)
    VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
  `);

  // Chaos incident
  insert.run(
    "evt-chaos-1", "chaos_triggered", "buero-dev-1",
    JSON.stringify({ type: "power_outage", target_room: "buero-dev-1", description: "Stromausfall" }),
    "cor-1", null, "op-1", 4820, now - 600_000, "none",
  );

  // Action caused by chaos
  insert.run(
    "evt-action-1", "transit_started", "AGENT-03",
    JSON.stringify({ from_room: "buero-dev-1", to_room: "kueche-eg", duration_ms: 5000 }),
    "cor-1", "evt-chaos-1", "op-2", 4821, now - 590_000, "none",
  );

  // Transit completed (outcome)
  insert.run(
    "evt-outcome-1", "transit_completed", "AGENT-03",
    JSON.stringify({ room: "kueche-eg" }),
    "cor-1", "evt-action-1", "op-3", 4822, now - 580_000, "none",
  );

  // Consolidation failure
  insert.run(
    "evt-fail-1", "agent_consolidation_failed", "AGENT-12",
    JSON.stringify({ run_id: "run-42", agent_name: "Lisa", error: "timeout" }),
    "run-42", null, "op-4", 5000, now - 300_000, "none",
  );

  // Successful nightrun (should NOT show as incident)
  insert.run(
    "evt-nr-ok", "nightrun_completed", "nightrun",
    JSON.stringify({ run_id: "run-41", agents_consolidated: 15, agents_failed: 0, duration_ms: 120000 }),
    "run-41", null, "op-5", 4000, now - 7200_000, "none",
  );

  // Nightrun with failures (SHOULD show)
  insert.run(
    "evt-nr-fail", "nightrun_completed", "nightrun",
    JSON.stringify({ run_id: "run-42", agents_consolidated: 12, agents_failed: 3, duration_ms: 180000 }),
    "run-42", null, "op-6", 5100, now - 200_000, "none",
  );

  // Agent despawned (unexpected)
  insert.run(
    "evt-despawn-1", "agent_despawned", "AGENT-07",
    JSON.stringify({ reason: "health_check_failed" }),
    "cor-2", null, "op-7", 5200, now - 100_000, "none",
  );
}

function seedEvolution(db: Database): void {
  const insert = db.prepare(`
    INSERT INTO personality_evolution (agent_id, tick, field, change_type,
                                       old_value, new_value, reason,
                                       nmda_score, source, created_at_ms)
    VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
  `);

  insert.run("AGENT-12", 4900, "conscientiousness", "drift", "0.85", "0.70", "drift_algorithm", 0.6, "realtime_judge", now - 400_000);
  insert.run("AGENT-05", 5050, "energy", "fatigue_spike", "0.60", "0.20", "fatigue_algorithm", 0.8, "realtime_judge", now - 250_000);
}

function seedProjection(db: Database): void {
  db.run("INSERT INTO projection_offsets VALUES ('sentinel-projection', 100)");
}

beforeAll(() => {
  projDb = new Database(":memory:");
  esDb = new Database(":memory:");

  projDb.exec(PROJECTION_SCHEMA);
  esDb.exec(EVENTS_SCHEMA);
  esDb.exec(EVOLUTION_SCHEMA);

  seedEvents(esDb);
  seedEvolution(esDb);
  seedProjection(projDb);

  setDatabases(projDb, esDb);
});

afterAll(() => {
  projDb.close();
  esDb.close();
});

// ── Tests ─────────────────────────────────────────

describe("GET /api/cockpit", () => {
  test("returns incidents with correct shape (AC-1)", async () => {
    const res = await app.request("/api/cockpit");
    expect(res.status).toBe(200);

    const data = await res.json();
    expect(data).toHaveProperty("incidents");
    expect(data).toHaveProperty("slo_violations");
    expect(data).toHaveProperty("total_active");
    expect(data).toHaveProperty("total_resolved_24h");
    expect(Array.isArray(data.incidents)).toBe(true);
    expect(Array.isArray(data.slo_violations)).toBe(true);

    // Every incident must have required fields (AC-1: only decidable states)
    for (const incident of data.incidents) {
      expect(incident).toHaveProperty("id");
      expect(incident).toHaveProperty("incident_type");
      expect(incident).toHaveProperty("severity");
      expect(incident).toHaveProperty("status");
      expect(incident).toHaveProperty("summary");
      expect(incident).toHaveProperty("actions");
      expect(incident).toHaveProperty("outcome");
      expect(["critical", "high", "medium", "low"]).toContain(incident.severity);
      expect(["active", "resolved", "pending", "failed"]).toContain(incident.status);
    }
  });

  test("incidents sorted by severity then timestamp (AC-2)", async () => {
    const res = await app.request("/api/cockpit");
    const data = await res.json();
    const incidents = data.incidents;

    const severityOrder = { critical: 0, high: 1, medium: 2, low: 3 };

    for (let i = 1; i < incidents.length; i++) {
      const prev = incidents[i - 1];
      const curr = incidents[i];
      const prevSev = severityOrder[prev.severity as keyof typeof severityOrder];
      const currSev = severityOrder[curr.severity as keyof typeof severityOrder];
      // Higher severity first, then newer timestamp first within same severity
      if (prevSev === currSev) {
        expect(prev.timestamp_ms).toBeGreaterThanOrEqual(curr.timestamp_ms);
      } else {
        expect(prevSev).toBeLessThanOrEqual(currSev);
      }
    }
  });

  test("chaos incident has linked actions (AC-3)", async () => {
    const res = await app.request("/api/cockpit");
    const data = await res.json();
    const chaosIncident = data.incidents.find(
      (i: { incident_type: string }) => i.incident_type === "chaos_triggered",
    );

    expect(chaosIncident).toBeDefined();
    expect(chaosIncident.actions.length).toBeGreaterThan(0);
    // Action should be transit_started caused by chaos
    expect(chaosIncident.actions[0].event_type).toBe("transit_started");
  });

  test("resolved incident has outcome (AC-4)", async () => {
    const res = await app.request("/api/cockpit");
    const data = await res.json();
    const chaosIncident = data.incidents.find(
      (i: { incident_type: string }) => i.incident_type === "chaos_triggered",
    );

    expect(chaosIncident).toBeDefined();
    expect(chaosIncident.status).toBe("resolved");
    expect(chaosIncident.outcome).not.toBeNull();
  });

  test("no metric wall - response is list-based (AC-N1)", async () => {
    const res = await app.request("/api/cockpit");
    const data = await res.json();
    // AC-N1: incidents must be an array (list), not a card-grid object
    expect(Array.isArray(data.incidents)).toBe(true);
    // Every incident is a flat item, not a nested category/group structure
    for (const incident of data.incidents) {
      expect(typeof incident.summary).toBe("string");
      expect(Array.isArray(incident.actions)).toBe(true);
    }
  });

  test("evolution alerts included as incidents", async () => {
    const res = await app.request("/api/cockpit");
    const data = await res.json();

    const evolutionIncidents = data.incidents.filter(
      (i: { source: string }) => i.source === "evolution",
    );
    expect(evolutionIncidents.length).toBeGreaterThanOrEqual(2);

    const fatigue = evolutionIncidents.find(
      (i: { incident_type: string }) => i.incident_type === "fatigue_spike",
    );
    expect(fatigue).toBeDefined();
    expect(fatigue.severity).toBe("high");
  });

  test("successful nightrun filtered out", async () => {
    const res = await app.request("/api/cockpit");
    const data = await res.json();

    // The nightrun_completed with 0 failures should NOT appear (filtered as low severity)
    const nightruns = data.incidents.filter(
      (i: { incident_type: string; id: string }) =>
        i.incident_type === "nightrun_completed" && i.id === "evt-nr-ok",
    );
    expect(nightruns.length).toBe(0);
  });
});

describe("GET /api/cockpit/incident/:id", () => {
  test("returns incident detail (AC-3, AC-4)", async () => {
    const res = await app.request("/api/cockpit/incident/evt-chaos-1");
    expect(res.status).toBe(200);

    const incident = await res.json();
    expect(incident.id).toBe("evt-chaos-1");
    expect(incident.incident_type).toBe("chaos_triggered");
    expect(incident.actions.length).toBeGreaterThan(0);
    expect(incident.outcome).not.toBeNull();
  });

  test("returns 404 for unknown incident", async () => {
    const res = await app.request("/api/cockpit/incident/nonexistent");
    expect(res.status).toBe(404);
  });
});

describe("empty database", () => {
  test("returns empty cockpit on fresh DB", async () => {
    const emptyEs = new Database(":memory:");
    const emptyProj = new Database(":memory:");
    emptyEs.exec(EVENTS_SCHEMA);
    emptyProj.exec(PROJECTION_SCHEMA);
    emptyProj.run("INSERT INTO projection_offsets VALUES ('sentinel-projection', 0)");

    setDatabases(emptyProj, emptyEs);

    const res = await app.request("/api/cockpit");
    expect(res.status).toBe(200);

    const data = await res.json();
    expect(data.incidents).toEqual([]);
    expect(data.total_active).toBe(0);
    expect(data.total_resolved_24h).toBe(0);

    // Restore original DBs
    setDatabases(projDb, esDb);

    emptyEs.close();
    emptyProj.close();
  });
});
