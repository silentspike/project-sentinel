import { describe, it, expect } from "vitest";
import { uuidFieldToString, frameHeaderSize } from "../src/transport/codec";
import { appendEventRows, EVENT_LOG_CAP, ingestFrame, consoleStore, type EventLogRow } from "../src/stores/console";

describe("codec helpers", () => {
  it("uuidFieldToString maps 16 raw bytes to hyphenated UUID", () => {
    const bytes = new Uint8Array([
      0x01, 0x9e, 0x7d, 0x9f, 0x27, 0x2b, 0x7a, 0x23, 0xba, 0x75, 0x60, 0xbc, 0x56, 0x7b, 0xd9, 0x6e,
    ]);
    expect(uuidFieldToString(bytes)).toBe("019e7d9f-272b-7a23-ba75-60bc567bd96e");
  });

  it("passes through string UUIDs", () => {
    expect(uuidFieldToString("abc-123")).toBe("abc-123");
  });

  it("frameHeaderSize = 2 + topic + 5", () => {
    expect(frameHeaderSize(2)).toBe(9);
    expect(frameHeaderSize(10)).toBe(17);
  });
});

describe("store reconcile (delta-merge from pushed frames)", () => {
  it("ingestFrame('agent_live') reconciles agents by key + updates topic/count", () => {
    ingestFrame("agent_live", {
      agents: [{ agent_id: 1, name: "Thomas", role: "CEO", current_room: "buero-ceo", energy: 0.8, stress: 0.2, mood: "ok" }],
    });
    expect(consoleStore.lastTopic).toBe("agent_live");
    expect(consoleStore.agents).toHaveLength(1);
    expect(consoleStore.agents[0].name).toBe("Thomas");

    // Zweiter Push: gleicher key (agent_id=1) gemerged, neuer Agent angehaengt.
    ingestFrame("agent_live", {
      agents: [
        { agent_id: 1, name: "Thomas", role: "CEO", current_room: "kueche", energy: 0.5, stress: 0.6, mood: "muede" },
        { agent_id: 5, name: "Andreas", role: "Lead", current_room: "buero-dev-1", energy: 0.7, stress: 0.3, mood: "ok" },
      ],
    });
    expect(consoleStore.agents).toHaveLength(2);
    expect(consoleStore.agents[0].current_room).toBe("kueche");
    expect(consoleStore.agents[1].name).toBe("Andreas");
  });

  it("ingestFrame('hello') stores the hello payload", () => {
    ingestFrame("hello", { server: "sentinel-dashboard-backend", proto: "topic-msgpack-zstd-v1" });
    expect(consoleStore.lastHello).toEqual({ server: "sentinel-dashboard-backend", proto: "topic-msgpack-zstd-v1" });
  });

  it("ingestFrame('room_live') reconciles rooms by room_id", () => {
    ingestFrame("room_live", {
      rooms: [
        {
          room_id: "kueche",
          occupant_count: 2,
          transit_count: 0,
          active_chaos: null,
          active_smells: null,
          temperature: 22.5,
          co2_ppm: 650,
          noise_db: 40,
          last_event_tick: 7,
        },
      ],
    });
    expect(consoleStore.lastTopic).toBe("room_live");
    expect(consoleStore.rooms).toHaveLength(1);
    expect(consoleStore.rooms[0].room_id).toBe("kueche");

    ingestFrame("room_live", {
      rooms: [
        {
          room_id: "kueche",
          occupant_count: 3,
          transit_count: 1,
          active_chaos: { type: "PrinterBroken" },
          active_smells: null,
          temperature: 23,
          co2_ppm: 700,
          noise_db: 45,
          last_event_tick: 8,
        },
        {
          room_id: "buero-ceo",
          occupant_count: 1,
          transit_count: 0,
          active_chaos: null,
          active_smells: null,
          temperature: 21,
          co2_ppm: 500,
          noise_db: 35,
          last_event_tick: 8,
        },
      ],
    });
    expect(consoleStore.rooms).toHaveLength(2);
    expect(consoleStore.rooms[0].occupant_count).toBe(3);
    expect(consoleStore.rooms[1].room_id).toBe("buero-ceo");
  });

  it("ingestFrame('kpi') stores the latest KPI bucket", () => {
    ingestFrame("kpi", {
      kpi: {
        bucket_start: 1000,
        active_agents: 12,
        total_actions: 30,
        total_transits: 4,
        chaos_events: 1,
        tick_count: 60,
        shift_changes: 0,
        nightrun_events: 0,
        updated_at: 1100,
      },
    });
    expect(consoleStore.lastTopic).toBe("kpi");
    expect(consoleStore.kpi?.active_agents).toBe(12);
    expect(consoleStore.kpi?.total_actions).toBe(30);
  });

  it("ingestFrame('event_log') appends by id, dedupes, and keeps ascending order", () => {
    ingestFrame("event_log", {
      events: [
        eventRow(12, "chaos_triggered", "kueche", { event_type: "AirConBroken", target_room: "kueche" }),
        eventRow(10, "agent_spawned", "1", { name: "Thomas" }),
      ],
    });
    ingestFrame("event_log", {
      events: [
        eventRow(12, "chaos_triggered", "kueche", { event_type: "PrinterBroken", target_room: "kueche" }),
        eventRow(13, "agent_action_received", "1", { content: "Ich pruefe den Drucker", target_room: "kueche" }),
      ],
    });
    const tail = consoleStore.events.slice(-3);
    expect(tail.map((event) => event.id)).toEqual([10, 12, 13]);
    expect(tail[1].payload).toContain("PrinterBroken");
    expect(consoleStore.lastTopic).toBe("event_log");
  });

  it("appendEventRows caps the in-memory event log at 10k rows", () => {
    const rows = Array.from({ length: EVENT_LOG_CAP + 5 }, (_, index) =>
      eventRow(index + 1, "tick_snapshot", "runtime", { tick: index + 1 }),
    );
    const capped = appendEventRows([], rows);
    expect(capped).toHaveLength(EVENT_LOG_CAP);
    expect(capped[0].id).toBe(6);
    expect(capped.at(-1)?.id).toBe(EVENT_LOG_CAP + 5);
  });
});

function eventRow(id: number, eventType: string, aggregateId: string, payload: Record<string, unknown>): EventLogRow {
  return {
    id,
    event_id: `evt-${id}`,
    event_type: eventType,
    aggregate_id: aggregateId,
    payload: JSON.stringify(payload),
    correlation_id: "",
    causation_id: null,
    tick: id * 10,
    timestamp_ms: 1_700_000_000_000 + id,
    compensation_type: "none",
  };
}
