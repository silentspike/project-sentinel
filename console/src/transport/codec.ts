import { decode as msgpackDecode } from "@msgpack/msgpack";
import { decompress as zstdDecompress } from "fzstd";

// Wire-Codec der Sentinel-Konsole (#419) — kompatibel zum #431-Backend (services/sentinel-dashboard-backend/src/codec.rs)
// und zum noaide-Frame-Format. Frame:
//   [2B topic_len BE][topic UTF-8][1B codec_id][4B payload_len BE][zstd(msgpack) payload]
// codec_id 0x01 = msgpack. payload_len = Laenge der KOMPRIMIERTEN Bytes.

export const CODEC_MSGPACK = 0x01;

export interface TopicFrame<T = unknown> {
  topic: string;
  value: T;
}

/** Minimale Frame-Groesse (Header) fuer ein gegebenes topic_len. */
export function frameHeaderSize(topicLen: number): number {
  return 2 + topicLen + 5;
}

/**
 * Parst einen topic-praefixierten Wire-Frame: dekomprimiert (zstd) + dekodiert (msgpack).
 * Wirft bei unbekanntem Codec.
 */
export function parseTopicFrame<T = unknown>(data: Uint8Array): TopicFrame<T> {
  const view = new DataView(data.buffer, data.byteOffset, data.byteLength);
  const topicLen = view.getUint16(0);
  const topic = new TextDecoder().decode(data.subarray(2, 2 + topicLen));

  const frameStart = 2 + topicLen;
  const codecId = data[frameStart];
  const payloadLen = view.getUint32(frameStart + 1);
  const compressed = data.subarray(frameStart + 5, frameStart + 5 + payloadLen);

  if (codecId !== CODEC_MSGPACK) {
    throw new Error(`Unsupported codec: 0x${codecId.toString(16)}`);
  }
  const decompressed = zstdDecompress(compressed);
  const value = msgpackDecode(decompressed) as T;
  return { topic, value };
}

/**
 * UUID-Feld aus MessagePack zu Hyphen-String. rmp-serde serialisiert `Uuid` als 16 Roh-Bytes
 * (non-human-readable), die JS als `Uint8Array` dekodiert.
 */
export function uuidFieldToString(value: unknown): string {
  if (typeof value === "string") return value;
  if (value instanceof Uint8Array && value.length === 16) {
    const h = Array.from(value, (b) => b.toString(16).padStart(2, "0")).join("");
    return `${h.slice(0, 8)}-${h.slice(8, 12)}-${h.slice(12, 16)}-${h.slice(16, 20)}-${h.slice(20)}`;
  }
  return String(value);
}
