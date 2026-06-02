import { parseTopicFrame, frameHeaderSize } from "./codec";
import {
  hashFromKey,
  readJsonFrame,
  reassembleEventLogCas,
  writeJsonFrame,
  type EventLogCasResponse,
} from "./cas";

// WebTransport-Client der Sentinel-Konsole (#419) — Port von noaide `transport/client.ts`.
// Server->Client unidirektionale Streams; topic+msgpack+zstd Frames. Self-signed Cert via
// /api/cert-hash + serverCertificateHashes (TLS-Primaerpfad, #431). Exponential-backoff Reconnect.

export type ConnectionStatus = "connecting" | "connected" | "disconnected";

interface TransportClientOptions {
  url: string;
  certHashUrl?: string;
  ticketUrl?: string;
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
  private readonly ticketUrl: string;
  private status: ConnectionStatus = "disconnected";
  private reconnectAttempt = 0;
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private abortController: AbortController | null = null;
  private readonly casBlocks = new Map<string, Uint8Array>();
  private casSyncInFlight = false;
  private casSyncQueued = false;
  private readonly onFrame?: (topic: string, value: unknown) => void;
  private readonly onStatusChange?: (status: ConnectionStatus) => void;

  constructor(options: TransportClientOptions) {
    this.url = options.url;
    this.certHashUrl = options.certHashUrl ?? `${window.location.origin}/api/cert-hash`;
    this.ticketUrl = options.ticketUrl ?? `${window.location.origin}/api/wt-ticket`;
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
      // WT-Auth via Einmal-Ticket in der URL (Browser senden bei WebTransport keine Cookies).
      const ticket = await this.fetchTicket();
      const wtUrl = ticket ? `${this.url}${this.url.includes("?") ? "&" : "?"}t=${encodeURIComponent(ticket)}` : this.url;
      this.transport = new WebTransport(wtUrl, options);
      await this.transport.ready;
      this.setStatus("connected");
      this.reconnectAttempt = 0;
      void this.readStreams();
      void this.syncEventLogCas();
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

  async syncEventLogCas(): Promise<void> {
    if (this.casSyncInFlight) {
      this.casSyncQueued = true;
      return;
    }
    this.casSyncInFlight = true;
    try {
      do {
        this.casSyncQueued = false;
        const transport = this.transport;
        if (!transport || this.status !== "connected") return;
        await this.performEventLogCasSync(transport);
      } while (this.casSyncQueued);
    } catch (e) {
      if (!this.abortController?.signal.aborted) console.warn("[transport] event_log CAS sync failed:", e);
    } finally {
      this.casSyncInFlight = false;
    }
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

  private async fetchTicket(): Promise<string | null> {
    try {
      const resp = await fetch(this.ticketUrl, { credentials: "include" });
      if (!resp.ok) return null;
      return ((await resp.json()) as { ticket?: string }).ticket ?? null;
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
            if (topic === "event_log_cas_tick") void this.syncEventLogCas();
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

  private async performEventLogCasSync(transport: WebTransport): Promise<void> {
    const stream = await transport.createBidirectionalStream();
    const writer = stream.writable.getWriter();
    const reader = stream.readable.getReader();
    try {
      await writeJsonFrame(writer, { have: [...this.casBlocks.keys()].map(hashFromKey) });
      await writer.close();
      const response = await readJsonFrame<EventLogCasResponse>(reader);
      const { events, stats } = reassembleEventLogCas(response, this.casBlocks);
      this.onFrame?.("event_log", { events, backfill: true, cas: stats });
    } finally {
      writer.releaseLock();
      reader.releaseLock();
    }
  }
}
