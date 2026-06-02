import { describe, expect, it } from "vitest";
import {
  hashKey,
  readJsonFrame,
  reassembleEventLogCas,
  writeJsonFrame,
  type BlockHash,
  type EventLogCasResponse,
} from "../src/transport/cas";

const encoder = new TextEncoder();

function hash(seed: number): BlockHash {
  return Array.from({ length: 16 }, (_, index) => seed + index);
}

function response(manifest: BlockHash[], blocks: [BlockHash, Uint8Array][]): EventLogCasResponse {
  return {
    topic: "event_log_cas",
    manifest,
    delta: {
      missing: blocks.map(([blockHash]) => blockHash),
      blocks: blocks.map(([blockHash, bytes]) => [blockHash, Array.from(bytes)]),
    },
    stats: {
      event_count: manifest.length,
      max_event_id: manifest.length,
      full_state_bytes: 1_000,
      delta_transfer_bytes: blocks.reduce((sum, [, bytes]) => sum + bytes.length, 0),
      known_blocks: 0,
      total_blocks: manifest.length,
      unique_blocks: new Set(manifest.map(hashKey)).size,
      dedup_ratio: 0,
      savings_ratio: 0,
    },
  };
}

describe("event-log CAS transport", () => {
  it("reassembles an ordered manifest from delta blocks and cached blocks", () => {
    const h1 = hash(1);
    const h2 = hash(50);
    const b1 = encoder.encode(`${JSON.stringify({ id: 1, event_type: "agent_spawned" })}\n`);
    const b2 = encoder.encode(`${JSON.stringify({ id: 2, event_type: "chaos_triggered" })}\n`);
    const cache = new Map<string, Uint8Array>();

    const first = reassembleEventLogCas(response([h1, h2], [[h1, b1], [h2, b2]]), cache, (bytes) => bytes);
    expect(first.events.map((event) => (event as { id: number }).id)).toEqual([1, 2]);
    expect(cache.has(hashKey(h1))).toBe(true);
    expect(cache.has(hashKey(h2))).toBe(true);

    const second = reassembleEventLogCas(response([h1, h2, h1], []), cache, (bytes) => bytes);
    expect(second.events.map((event) => (event as { id: number }).id)).toEqual([1, 2, 1]);
  });

  it("reads byte-compatible length-prefixed JSON frames across chunks", async () => {
    const chunks: Uint8Array[] = [];
    const writer = new WritableStream<Uint8Array>({
      write(chunk) {
        chunks.push(chunk);
      },
    }).getWriter();
    await writeJsonFrame(writer, { have: [hash(3)] });

    const frame = chunks[0];
    const stream = new ReadableStream<Uint8Array>({
      start(controller) {
        controller.enqueue(frame.slice(0, 2));
        controller.enqueue(frame.slice(2, 9));
        controller.enqueue(frame.slice(9));
        controller.close();
      },
    });

    await expect(readJsonFrame(stream.getReader())).resolves.toEqual({ have: [hash(3)] });
  });
});
