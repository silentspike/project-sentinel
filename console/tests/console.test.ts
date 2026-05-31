import { describe, it, expect } from "vitest";
import { uuidFieldToString, frameHeaderSize } from "../src/transport/codec";
import { ingestFrame, consoleStore } from "../src/stores/console";

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
});
