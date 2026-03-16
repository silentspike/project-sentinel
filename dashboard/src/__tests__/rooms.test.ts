import { afterAll, beforeAll, describe, expect, it } from "bun:test";
import { Database } from "bun:sqlite";
import { app } from "../index";
import { closeDatabases, setDatabases } from "../db";

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
      hunger REAL DEFAULT 0,
      energy REAL DEFAULT 0,
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

  projDb.run(
    `INSERT INTO room_live_view
      (room_id, occupant_count, transit_count, active_chaos, active_smells, temperature, co2_ppm, noise_db, last_event_tick, last_event_id, updated_at)
     VALUES
      ('kueche', 2, 0, '{"event_type":"AirConBroken","description":"Klimaanlage defekt"}', NULL, 25.4, 640, 54, 120, 10, 1000),
      ('empfang', 0, 0, NULL, NULL, 21.0, 420, 39, 120, 9, 1000)`,
  );
  projDb.run(
    `INSERT INTO agent_live_view
      (agent_id, name, role, shift_set, status, current_room, in_transit, transit_target, last_action, last_action_tick, hunger, energy, stress, bladder, social_need, caffeine_mg, mood, last_event_id, updated_at)
     VALUES
      (1, 'Anna Schmidt', 'developer', 1, 'active', 'kueche', 0, NULL, 'Kaffee holen', 118, 20, 70, 15, 5, 30, 80, 'Focused', 11, 1000),
      (2, 'Max Weber', 'designer', 1, 'active', 'kueche', 0, NULL, 'Drucker pruefen', 119, 25, 65, 20, 5, 25, 30, 'Neutral', 12, 1000)`,
  );
  projDb.run(
    "INSERT INTO kpi_1m VALUES (1000, 2, 12, 3, 1, 120, 0, 0, 12, 1000)",
  );

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
    "INSERT INTO projection_offsets VALUES ('sentinel-projection', 4, 1000)",
  );
  esDb.run(
    `INSERT INTO events
      (id, event_id, event_type, aggregate_id, payload, correlation_id, causation_id, operation_id, tick, timestamp_ms)
     VALUES
      (1, 'chaos-1', 'chaos_triggered', 'kueche', '{"event_type":"AirConBroken","target_room":"kueche","description":"Klimaanlage defekt"}', 'corr-1', NULL, 'op-1', 110, 1010),
      (2, 'stimulus-1', 'room_stimulus_applied', 'kueche', '{"room_id":"kueche","stimulus_type":"co2","delta":900,"duration_ticks":90,"description":"CO2-Reiz +900 ppm"}', 'corr-2', NULL, 'op-2', 115, 1015),
      (3, 'physics-1', 'room_physics_updated', 'kueche', '{"room_id":"kueche","temperature":24.1,"co2_ppm":610,"noise_db":50.0,"occupant_count":2}', 'corr-3', NULL, 'op-3', 100, 1000),
      (4, 'physics-2', 'room_physics_updated', 'kueche', '{"room_id":"kueche","temperature":25.4,"co2_ppm":1540,"noise_db":54.0,"occupant_count":2}', 'corr-4', NULL, 'op-4', 120, 1020),
      (5, 'action-1', 'agent_action_received', '1', '{"agent_id":1,"action_type":"ToolUse","target_room":"kueche","content":"Die Luft ist stickig hier."}', 'corr-5', NULL, 'op-5', 121, 1030)`,
  );

  setDatabases(projDb, esDb);
});

afterAll(() => {
  closeDatabases();
});

describe("Room detail routes", () => {
  it("GET /api/rooms/:id/detail returns snapshot plus history", async () => {
    const res = await app.request("/api/rooms/kueche/detail");
    expect(res.status).toBe(200);

    const body = await res.json();
    expect(body.id).toBe("kueche");
    expect(body.occupants).toEqual(["Anna Schmidt", "Max Weber"]);
    expect(body.active_chaos).toEqual({
      event_type: "AirConBroken",
      description: "Klimaanlage defekt",
    });
    expect(body.physics_history).toHaveLength(2);
    expect(body.physics_history[0].tick).toBe(100);
    expect(body.physics_history[1].co2_ppm).toBe(1540);
    expect(body.chaos_history).toHaveLength(1);
    expect(body.chaos_history[0].chaos_type).toBe("AirConBroken");
    expect(body.stimulus_history).toHaveLength(1);
    expect(body.stimulus_history[0].stimulus_type).toBe("co2");
    expect(body.recent_reactions).toHaveLength(1);
    expect(body.recent_reactions[0].agent_name).toBe("Anna Schmidt");
    expect(body.reaction_window_ticks).toBe(60);
    expect(body.recent_reactions[0].chaos_type).toBe("AirConBroken");
    expect(body.recent_reactions[0].chaos_tick).toBe(110);
    expect(body.recent_reactions[0].stimulus_type).toBe("co2");
    expect(body.recent_reactions[0].stimulus_tick).toBe(115);
  });

  it("GET /api/rooms/:id/detail returns 404 for unknown room", async () => {
    const res = await app.request("/api/rooms/unbekannt/detail");
    expect(res.status).toBe(404);
  });

  it("uses projected room occupancy when agent status is stale and reactions omit target_room", async () => {
    projDb.run("UPDATE agent_live_view SET status = 'despawned'");
    projDb.run("UPDATE room_live_view SET occupant_count = 0 WHERE room_id = 'kueche'");
    projDb.run(
      `INSERT INTO room_live_view
        (room_id, occupant_count, transit_count, active_chaos, active_smells, temperature, co2_ppm, noise_db, last_event_tick, last_event_id, updated_at)
       VALUES
        ('buero-design-1', 2, 0, NULL, NULL, 29.9, 420, 30, 305, 50, 2000)`,
    );
    projDb.run(
      `INSERT INTO agent_live_view
        (agent_id, name, role, shift_set, status, current_room, in_transit, transit_target, last_action, last_action_tick, hunger, energy, stress, bladder, social_need, caffeine_mg, mood, last_event_id, updated_at)
       VALUES
        (10, 'Robin Krause', 'designer', 2, 'despawned', 'buero-design-1', 0, NULL, 'Luft faecheln', 305, 20, 60, 25, 5, 15, 0, 'Warm', 320, 2000),
        (11, 'Lisa Brenner', 'designer', 2, 'despawned', 'buero-design-1', 0, NULL, 'Stirn wischen', 304, 20, 60, 25, 5, 15, 0, 'Warm', 319, 2000),
        (12, 'Altlast Agent', 'designer', 2, 'despawned', 'buero-design-1', 0, NULL, 'Veraltet', 100, 20, 60, 25, 5, 15, 0, 'Neutral', 100, 2000)`,
    );
    esDb.run(
      `INSERT INTO events
        (id, event_id, event_type, aggregate_id, payload, correlation_id, causation_id, operation_id, tick, timestamp_ms)
       VALUES
        (6, 'stimulus-2', 'room_stimulus_applied', 'buero-design-1', '{"room_id":"buero-design-1","stimulus_type":"temperature","delta":8,"duration_ticks":180,"description":"Temperaturreiz +8.0 °C"}', 'corr-6', NULL, 'op-6', 294, 2001),
        (7, 'action-2', 'agent_action_received', 'AGENT-10', '{"agent_id":10,"action_type":"Emote","target_room":null,"content":"*faechelt sich mit der Hand Luft zu*"}', 'corr-7', NULL, 'op-7', 305, 2002),
        (8, 'action-3', 'agent_action_received', 'AGENT-11', '{"agent_id":11,"action_type":"Emote","target_room":null,"content":"*wischt sich kurz ueber die Stirn*"}', 'corr-8', NULL, 'op-8', 306, 2003),
        (9, 'action-4', 'agent_action_received', 'AGENT-12', '{"agent_id":12,"action_type":"Emote","target_room":null,"content":"*arbeitet ruhig weiter*"}', 'corr-9', NULL, 'op-9', 360, 2100)`,
    );
    setDatabases(projDb, esDb);

    const res = await app.request("/api/rooms/buero-design-1/detail");
    expect(res.status).toBe(200);

    const body = await res.json();
    expect(body.occupants).toEqual(["Robin Krause", "Lisa Brenner"]);
    expect(body.recent_reactions).toHaveLength(2);
    expect(body.recent_reactions[0].agent_name).toBe("Robin Krause");
    expect(body.recent_reactions[0].stimulus_type).toBe("temperature");
    expect(body.recent_reactions[1].agent_name).toBe("Lisa Brenner");
    expect(body.recent_reactions[1].stimulus_type).toBe("temperature");
  });

  it("keeps latest stimulus reactions visible when agents leave the room afterwards", async () => {
    projDb.run(
      `INSERT INTO room_live_view
        (room_id, occupant_count, transit_count, active_chaos, active_smells, temperature, co2_ppm, noise_db, last_event_tick, last_event_id, updated_at)
       VALUES
        ('buero-dev-2', 0, 0, NULL, NULL, 28.4, 520, 68, 430, 60, 3000)`,
    );
    projDb.run(
      `INSERT INTO agent_live_view
        (agent_id, name, role, shift_set, status, current_room, in_transit, transit_target, last_action, last_action_tick, hunger, energy, stress, bladder, social_need, caffeine_mg, mood, last_event_id, updated_at)
       VALUES
        (20, 'Fatima Noor', 'developer', 2, 'active', 'kueche', 0, NULL, 'Abkuehlen', 410, 20, 55, 35, 5, 10, 0, 'Irritated', 430, 3000)`,
    );
    esDb.run(
      `INSERT INTO events
        (id, event_id, event_type, aggregate_id, payload, correlation_id, causation_id, operation_id, tick, timestamp_ms)
       VALUES
        (10, 'stimulus-3', 'room_stimulus_applied', 'buero-dev-2', '{"room_id":"buero-dev-2","stimulus_type":"noise","delta":35,"duration_ticks":120,"description":"Laermreiz +35 dB"}', 'corr-10', NULL, 'op-10', 405, 3001),
        (11, 'action-5', 'agent_action_received', 'AGENT-20', '{"agent_id":20,"action_type":"Move","target_room":"kueche","content":"*schiebt den Stuhl zurueck und geht Richtung Kueche*"}', 'corr-11', NULL, 'op-11', 409, 3002),
        (12, 'transit-1', 'transit_started', 'AGENT-20', '{"from_room":"buero-dev-2","to_room":"kueche","duration_ms":2300}', 'corr-12', NULL, 'op-12', 410, 3003)`,
    );
    setDatabases(projDb, esDb);

    const res = await app.request("/api/rooms/buero-dev-2/detail");
    expect(res.status).toBe(200);

    const body = await res.json();
    expect(body.occupants).toEqual([]);
    expect(body.recent_reactions).toHaveLength(2);
    expect(body.recent_reactions[0].agent_name).toBe("Fatima Noor");
    expect(body.recent_reactions[0].stimulus_type).toBe("noise");
    expect(body.recent_reactions[0].target_room).toBe("kueche");
    expect(body.recent_reactions[1].action_type).toBe("Transit");
    expect(body.recent_reactions[1].content).toContain("kueche");
    expect(body.recent_reactions[1].stimulus_type).toBe("noise");
  });

  it("ignores future chaos ticks when selecting the current reaction context", async () => {
    projDb.run(
      `INSERT INTO room_live_view
        (room_id, occupant_count, transit_count, active_chaos, active_smells, temperature, co2_ppm, noise_db, last_event_tick, last_event_id, updated_at)
       VALUES
        ('buero-dev-3', 1, 0, '{"event_type":"FireAlarmDrill","description":"Uebung"}', NULL, 31.0, 620, 35, 125, 70, 4000)`,
    );
    projDb.run(
      `INSERT INTO agent_live_view
        (agent_id, name, role, shift_set, status, current_room, in_transit, transit_target, last_action, last_action_tick, hunger, energy, stress, bladder, social_need, caffeine_mg, mood, last_event_id, updated_at)
       VALUES
        (31, 'Robin Test', 'developer', 2, 'active', 'buero-dev-3', 0, NULL, 'Arbeitet', 125, 20, 60, 25, 5, 15, 0, 'Warm', 4010, 4000)`,
    );
    esDb.run(
      `INSERT INTO events
        (id, event_id, event_type, aggregate_id, payload, correlation_id, causation_id, operation_id, tick, timestamp_ms)
       VALUES
        (13, 'stimulus-4', 'room_stimulus_applied', 'buero-dev-3', '{"room_id":"buero-dev-3","stimulus_type":"temperature","delta":9,"duration_ticks":120,"description":"Temperaturreiz +9.0 °C"}', 'corr-13', NULL, 'op-13', 120, 4001),
        (14, 'chaos-future', 'chaos_triggered', 'buero-dev-3', '{"event_type":"FireAlarmDrill","target_room":"buero-dev-3","description":"Spaeteres Chaos"}', 'corr-14', NULL, 'op-14', 900, 9000),
        (15, 'action-6', 'agent_action_received', 'AGENT-31', '{"agent_id":31,"action_type":"Emote","target_room":null,"content":"*wischt sich ueber die Stirn*"}', 'corr-15', NULL, 'op-15', 124, 4002)`,
    );
    setDatabases(projDb, esDb);

    const res = await app.request("/api/rooms/buero-dev-3/detail");
    expect(res.status).toBe(200);

    const body = await res.json();
    expect(body.recent_reactions).toHaveLength(1);
    expect(body.recent_reactions[0].agent_name).toBe("Robin Test");
    expect(body.recent_reactions[0].stimulus_type).toBe("temperature");
    expect(body.recent_reactions[0].chaos_type).toBeNull();
  });
});
