import { describe, expect, it } from "vitest";
import { mergeRoomMeta, ROOM_METADATA, roomDisplayName, type RoomViewModel } from "../src/roomsMeta";
import type { AgentRow, RoomRow } from "../src/stores/console";

function agent(agent_id: number, overrides: Partial<AgentRow> = {}): AgentRow {
  return {
    agent_id,
    name: `Agent ${agent_id}`,
    role: "Developer",
    current_room: "buero-dev-1",
    status: "active",
    in_transit: false,
    mood: null,
    ...overrides,
  };
}

function room(overrides: Partial<RoomRow> = {}): RoomRow {
  return {
    room_id: "buero-dev-1",
    occupant_count: 4,
    transit_count: 2,
    active_chaos: null,
    active_smells: null,
    temperature: 22,
    co2_ppm: 600,
    noise_db: 40,
    last_event_tick: 12,
    ...overrides,
  };
}

describe("mergeRoomMeta on-site presence", () => {
  it.each(["suspended", "despawned", "sleeping", "errored", "recovery_required", "Active", "", undefined])(
    "excludes status %s instead of defaulting it to active",
    (status) => {
      expect(mergeRoomMeta(room(), [agent(1), agent(2, { status })]).occupants).toEqual(["Agent 1"]);
    },
  );

  it("excludes all transit rows from both departure and destination rooms", () => {
    const agents = [
      agent(1),
      agent(2, { in_transit: true, transit_target: "buero-dev-2" }),
      agent(3, { current_room: "buero-dev-2", in_transit: true, transit_target: "buero-dev-1" }),
      agent(4, { in_transit: true, transit_target: "buero-dev-1" }),
      agent(5, { current_room: null, in_transit: true, transit_target: "buero-dev-1" }),
    ];
    expect(mergeRoomMeta(room(), agents).occupants).toEqual(["Agent 1"]);
    expect(mergeRoomMeta(room({ room_id: "buero-dev-2" }), agents).occupants).toEqual([]);
  });

  it("matches the exact current room ID, not prefixes, labels, targets or whitespace", () => {
    const agents = [
      agent(1),
      agent(2, { current_room: "buero-dev-2" }),
      agent(3, { current_room: "buero-dev-1-extra" }),
      agent(4, { current_room: "buero-dev-1 " }),
      agent(5, { current_room: "BUERO-DEV-1" }),
      agent(6, { current_room: roomDisplayName("buero-dev-1") }),
      agent(7, { current_room: null, transit_target: "buero-dev-1" }),
    ];
    expect(mergeRoomMeta(room(), agents).occupants).toEqual(["Agent 1"]);
  });

  it("deduplicates eligible agent IDs without merging distinct agents with the same name", () => {
    const agents = [
      agent(1, { status: "suspended" }),
      agent(2, { name: "Same Name" }),
      agent(2, { name: "Duplicate row" }),
      agent(1, { name: "Same Name" }),
      agent(1, { name: "Duplicate row" }),
      agent(3, { in_transit: true }),
    ];
    expect(mergeRoomMeta(room(), agents).occupants).toEqual(["Same Name", "Same Name"]);
  });

  it("keeps active rows compatible when the optional transit flag is absent", () => {
    expect(mergeRoomMeta(room(), [agent(1, { in_transit: undefined })]).occupants).toEqual(["Agent 1"]);
    expect(mergeRoomMeta(room(), []).occupants).toEqual([]);
  });

  it("preserves server counts and room fields without computing arrivals or mutating inputs", () => {
    const source = room({ occupant_count: 9, transit_count: 7, updated_at: 123, extra: "retained" });
    const agents = [agent(1), agent(2, { in_transit: true, transit_target: source.room_id })];
    const beforeRoom = { ...source };
    const beforeAgents = agents.map((row) => ({ ...row }));
    Object.freeze(source);
    agents.forEach(Object.freeze);
    Object.freeze(agents);
    const merged: RoomViewModel = mergeRoomMeta(source, agents);
    expect(merged).toEqual({ ...source, ...ROOM_METADATA[source.room_id], id: source.room_id, occupants: ["Agent 1"] });
    expect(merged.occupant_count).toBe(9);
    expect(merged.transit_count).toBe(7);
    expect(source).toEqual(beforeRoom);
    expect(agents).toEqual(beforeAgents);
  });

  it("preserves the view-model fallback for rooms not present in metadata", () => {
    const source = room({ room_id: "new-room", occupant_count: 1, transit_count: 0 });
    expect(mergeRoomMeta(source, [agent(1, { current_room: "new-room" })])).toEqual({
      ...source,
      id: "new-room",
      name: "new-room",
      floor: 0,
      capacity: 0,
      room_type: "unknown",
      occupants: ["Agent 1"],
    });
    expect(roomDisplayName("new-room")).toBe("new-room");
  });
});
