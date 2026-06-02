import { decompress } from "fzstd";

export type BlockHash = number[];

export interface Delta {
  missing: BlockHash[];
  blocks: [BlockHash, number[]][];
}

export interface EventLogCasStats {
  event_count: number;
  max_event_id: number;
  full_state_bytes: number;
  delta_transfer_bytes: number;
  known_blocks: number;
  total_blocks: number;
  unique_blocks: number;
  dedup_ratio: number;
  savings_ratio: number;
}

export interface EventLogCasResponse {
  topic: "event_log_cas";
  manifest: BlockHash[];
  delta: Delta;
  stats: EventLogCasStats;
}

export interface ReassembledEventLog {
  events: unknown[];
  stats: EventLogCasStats;
}

type DecompressBlock = (bytes: Uint8Array) => Uint8Array;

const textEncoder = new TextEncoder();
const textDecoder = new TextDecoder();

export function hashKey(hash: BlockHash): string {
  return hash.join(",");
}

export function hashFromKey(key: string): BlockHash {
  if (!key) return [];
  return key.split(",").map((part) => Number(part));
}

export async function writeJsonFrame(writer: WritableStreamDefaultWriter<Uint8Array>, value: unknown): Promise<void> {
  const payload = textEncoder.encode(JSON.stringify(value));
  const frame = new Uint8Array(4 + payload.length);
  new DataView(frame.buffer, frame.byteOffset, frame.byteLength).setUint32(0, payload.length, true);
  frame.set(payload, 4);
  await writer.write(frame);
}

export async function readJsonFrame<T>(reader: ReadableStreamDefaultReader<Uint8Array>): Promise<T> {
  let buffer = new Uint8Array(0);
  let length: number | null = null;
  for (;;) {
    if (length === null && buffer.length >= 4) {
      length = new DataView(buffer.buffer, buffer.byteOffset, buffer.byteLength).getUint32(0, true);
    }
    if (length !== null && buffer.length >= 4 + length) {
      const payload = buffer.slice(4, 4 + length);
      return JSON.parse(textDecoder.decode(payload)) as T;
    }

    const { done, value } = await reader.read();
    if (done || !value) throw new Error("stream ended before full CAS frame");
    const merged = new Uint8Array(buffer.length + value.length);
    merged.set(buffer);
    merged.set(value, buffer.length);
    buffer = merged;
  }
}

export function reassembleEventLogCas(
  response: EventLogCasResponse,
  cache: Map<string, Uint8Array>,
  decompressBlock: DecompressBlock = decompress,
): ReassembledEventLog {
  for (const [hash, block] of response.delta.blocks) {
    cache.set(hashKey(hash), new Uint8Array(block));
  }

  let ndjson = "";
  for (const hash of response.manifest) {
    const block = cache.get(hashKey(hash));
    if (!block) throw new Error(`missing CAS block ${hashKey(hash)}`);
    ndjson += textDecoder.decode(decompressBlock(block), { stream: true });
  }
  ndjson += textDecoder.decode();

  const events = ndjson
    .split("\n")
    .filter((line) => line.trim().length > 0)
    .map((line) => JSON.parse(line));
  return { events, stats: response.stats };
}
