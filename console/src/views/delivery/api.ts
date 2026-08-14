import { parsePublicDeliveryLineageDto, type PublicDeliveryLineageDto } from "./lineage";

const MAX_LINEAGE_BYTES = 256 * 1024;
export const DELIVERY_LINEAGE_DEADLINE_MS = 5_000;
const JSON_CONTENT_TYPE = /^application\/json(?:\s*;[^\r\n]*)?$/i;

export class DeliveryLineageUnavailable extends Error {
  constructor() {
    super("Delivery lineage is unavailable");
    this.name = "DeliveryLineageUnavailable";
  }
}

export async function fetchPublicDeliveryLineage(
  signal?: AbortSignal,
): Promise<PublicDeliveryLineageDto> {
  const requestAbort = new AbortController();
  const abortFromCaller = () => requestAbort.abort(signal?.reason);
  if (signal?.aborted) abortFromCaller();
  else signal?.addEventListener("abort", abortFromCaller, { once: true });
  const deadline = setTimeout(
    () => requestAbort.abort(new Error("delivery lineage deadline exceeded")),
    DELIVERY_LINEAGE_DEADLINE_MS,
  );
  let response: Response;
  try {
    response = await fetch("/api/v1/delivery/lineage", {
      method: "GET",
      credentials: "include",
      headers: { accept: "application/json" },
      signal: requestAbort.signal,
    });
    const contentType = response.headers.get("content-type") ?? "";
    if (!response.ok || !JSON_CONTENT_TYPE.test(contentType)) {
      await response.body?.cancel();
      throw new DeliveryLineageUnavailable();
    }
    const declaredLength = response.headers.get("content-length");
    if (
      declaredLength !== null &&
      (!/^(?:0|[1-9][0-9]*)$/.test(declaredLength) || Number(declaredLength) > MAX_LINEAGE_BYTES)
    ) {
      await response.body?.cancel();
      throw new DeliveryLineageUnavailable();
    }
    const bytes = await readBoundedBody(response, requestAbort.signal);
    const text = new TextDecoder("utf-8", { fatal: true }).decode(bytes);
    return parsePublicDeliveryLineageDto(JSON.parse(text));
  } catch {
    throw new DeliveryLineageUnavailable();
  } finally {
    clearTimeout(deadline);
    signal?.removeEventListener("abort", abortFromCaller);
  }
}

async function readBoundedBody(response: Response, signal: AbortSignal): Promise<Uint8Array> {
  if (!response.body) return new Uint8Array();
  const reader = response.body.getReader();
  const chunks: Uint8Array[] = [];
  let total = 0;
  try {
    while (true) {
      const { done, value } = await readWithAbort(reader, signal);
      if (done) break;
      total += value.byteLength;
      if (total > MAX_LINEAGE_BYTES) {
        await reader.cancel();
        throw new DeliveryLineageUnavailable();
      }
      chunks.push(value);
    }
  } finally {
    reader.releaseLock();
  }
  const joined = new Uint8Array(total);
  let offset = 0;
  for (const chunk of chunks) {
    joined.set(chunk, offset);
    offset += chunk.byteLength;
  }
  return joined;
}

async function readWithAbort(
  reader: ReadableStreamDefaultReader<Uint8Array>,
  signal: AbortSignal,
): Promise<ReadableStreamReadResult<Uint8Array>> {
  if (signal.aborted) {
    await reader.cancel();
    throw new DeliveryLineageUnavailable();
  }
  return new Promise((resolve, reject) => {
    const onAbort = () => {
      void reader.cancel().finally(() => reject(new DeliveryLineageUnavailable()));
    };
    signal.addEventListener("abort", onAbort, { once: true });
    void reader.read().then(
      (result) => {
        signal.removeEventListener("abort", onAbort);
        resolve(result);
      },
      (error: unknown) => {
        signal.removeEventListener("abort", onAbort);
        reject(error);
      },
    );
  });
}
