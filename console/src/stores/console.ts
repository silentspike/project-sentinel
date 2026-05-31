import { createStore, reconcile } from "solid-js/store";
import { createSignal } from "solid-js";
import { TransportClient, type ConnectionStatus } from "../transport/client";

// Reaktiver Konsolen-State (#419): createStore + reconcile (key-basierter Delta-Merge) + Version-Signals
// (noaide-Muster). Der TransportClient pusht topic-Frames; pro Topic wird der Store reaktiv gemerged.

export interface AgentRow {
  agent_id: number;
  name: string;
  role: string;
  current_room: string | null;
  energy: number;
  stress: number;
  mood: string | null;
  [k: string]: unknown;
}

interface ConsoleState {
  agents: AgentRow[];
  lastTopic: string | null;
  lastHello: Record<string, unknown> | null;
}

const [state, setState] = createStore<ConsoleState>({
  agents: [],
  lastTopic: null,
  lastHello: null,
});

const [status, setStatus] = createSignal<ConnectionStatus>("disconnected");
const [frameCount, setFrameCount] = createSignal(0);

/** Verarbeitet einen gepushten topic-Frame in den reaktiven Store. */
export function ingestFrame(topic: string, value: unknown) {
  setState("lastTopic", topic);
  setFrameCount((n) => n + 1);
  if (topic === "hello") {
    setState("lastHello", value as Record<string, unknown>);
  } else if (topic === "agent_live" && value && typeof value === "object") {
    const rows = (value as { agents?: AgentRow[] }).agents;
    if (Array.isArray(rows)) setState("agents", reconcile(rows, { key: "agent_id" }));
  }
}

let client: TransportClient | null = null;

/** Verbindet den WebTransport-Client (idempotent). */
export function connectTransport(url: string): TransportClient {
  if (client) return client;
  client = new TransportClient({
    url,
    onFrame: ingestFrame,
    onStatusChange: setStatus,
  });
  void client.connect();
  return client;
}

export const consoleStore = state;
export { status, frameCount, setStatus };
