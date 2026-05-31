import { parseTopicFrame, frameHeaderSize } from "./codec";

// WebTransport-Client der Sentinel-Konsole (#419) — Port von noaide `transport/client.ts`.
// Server->Client unidirektionale Streams; topic+msgpack+zstd Frames. Self-signed Cert via
// /api/cert-hash + serverCertificateHashes (TLS-Primaerpfad, #431). Exponential-backoff Reconnect.

export type ConnectionStatus = "connecting" | "connected" | "disconnected";

interface TransportClientOptions {
  url: string;
  certHashUrl?: string;
  onFrame?: (topic: string, value: unknown) => void;
  onStatusChange?: (status: ConnectionStatus) => void;
}

const MAX_BACKOFF_MS = 30_000;
const INITIAL_BACKOFF_MS = 500;

function base64ToArrayBuffer(base64: string): ArrayBuffer {
  const binary = atob(base64);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i);
  return bytes.buffer;
}

export class TransportClient {
  private transport: WebTransport | null = null;
  private readonly url: string;
  private readonly certHashUrl: string;
  private status: ConnectionStatus = "disconnected";
  private reconnectAttempt = 0;
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private abortController: AbortController | null = null;
  private readonly onFrame?: (topic: string, value: unknown) => void;
  private readonly onStatusChange?: (status: ConnectionStatus) => void;

  constructor(options: TransportClientOptions) {
    this.url = options.url;
    this.certHashUrl = options.certHashUrl ?? `${window.location.origin}/api/cert-hash`;
    this.onFrame = options.onFrame;
    this.onStatusChange = options.onStatusChange;
  }

  async connect(): Promise<void> {
    if (this.status === "connecting" || this.status === "connected") return;
    this.abortController = new AbortController();
    this.setStatus("connecting");
    try {
      const certHash = await this.fetchCertHash();
      const options: WebTransportOptions = {};
      if (certHash) {
        options.serverCertificateHashes = [{ algorithm: "sha-256", value: base64ToArrayBuffer(certHash) }];
      }
      this.transport = new WebTransport(this.url, options);
      await this.transport.ready;
      this.setStatus("connected");
      this.reconnectAttempt = 0;
      void this.readStreams();
      this.transport.closed
        .then(() => this.onClosed())
        .catch(() => this.onClosed());
    } catch (e) {
      console.warn("[transport] connection failed:", e);
      this.setStatus("disconnected");
      this.scheduleReconnect();
    }
  }

  disconnect(): void {
    this.abortController?.abort();
    this.abortController = null;
    if (this.reconnectTimer) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
    this.transport?.close();
    this.transport = null;
    this.setStatus("disconnected");
    this.reconnectAttempt = 0;
  }

  currentStatus(): ConnectionStatus {
    return this.status;
  }

  private onClosed() {
    this.setStatus("disconnected");
    this.scheduleReconnect();
  }

  private setStatus(status: ConnectionStatus) {
    this.status = status;
    this.onStatusChange?.(status);
  }

  private async fetchCertHash(): Promise<string | null> {
    try {
      const resp = await fetch(this.certHashUrl, { credentials: "include" });
      if (!resp.ok) return null;
      const data = (await resp.json()) as { algorithm: string; hash: string | null };
      return data.hash ?? null;
    } catch {
      return null;
    }
  }

  private scheduleReconnect() {
    if (this.abortController?.signal.aborted) return;
    const backoff = Math.min(INITIAL_BACKOFF_MS * 2 ** this.reconnectAttempt, MAX_BACKOFF_MS);
    this.reconnectAttempt++;
    this.reconnectTimer = setTimeout(() => void this.connect(), backoff);
  }

  private async readStreams() {
    if (!this.transport) return;
    const reader = this.transport.incomingUnidirectionalStreams.getReader();
    try {
      for (;;) {
        const { done, value: stream } = await reader.read();
        if (done) break;
        void this.readStream(stream);
      }
    } catch (e) {
      if (!this.abortController?.signal.aborted) console.warn("[transport] stream reader error:", e);
    }
  }

  private async readStream(stream: ReadableStream<Uint8Array>) {
    const reader = stream.getReader();
    let buffer = new Uint8Array(0);
    try {
      for (;;) {
        const { done, value } = await reader.read();
        if (done) break;
        const merged = new Uint8Array(buffer.length + value.length);
        merged.set(buffer);
        merged.set(value, buffer.length);
        buffer = merged;

        // Vollstaendige Frames aus dem Buffer extrahieren.
        while (buffer.length >= 2) {
          const view = new DataView(buffer.buffer, buffer.byteOffset, buffer.byteLength);
          const topicLen = view.getUint16(0);
          const headerSize = frameHeaderSize(topicLen);
          if (buffer.length < headerSize) break;
          const payloadLen = view.getUint32(2 + topicLen + 1);
          const totalSize = headerSize + payloadLen;
          if (buffer.length < totalSize) break;
          const frame = buffer.slice(0, totalSize);
          buffer = buffer.subarray(totalSize);
          try {
            const { topic, value: decoded } = parseTopicFrame(frame);
            this.onFrame?.(topic, decoded);
          } catch (e) {
            console.warn("[transport] frame decode error:", e);
          }
        }
      }
    } catch (e) {
      if (!this.abortController?.signal.aborted) console.warn("[transport] stream read error:", e);
    }
  }
}
