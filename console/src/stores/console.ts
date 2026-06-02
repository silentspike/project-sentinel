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
  shift_set?: number;
  status?: string;
  in_transit?: boolean;
  transit_target?: string | null;
  last_action?: string | null;
  last_action_tick?: number | null;
  hunger?: number;
  energy?: number;
  stress?: number;
  bladder?: number;
  social_need?: number;
  caffeine_mg?: number;
  mood: string | null;
  last_event_id?: number;
  updated_at?: number;
  stalled?: boolean;
  [k: string]: unknown;
}

export interface RoomRow {
  room_id: string;
  occupant_count: number;
  transit_count: number;
  active_chaos: unknown | null;
  active_smells: unknown | null;
  temperature: number | null;
  co2_ppm: number | null;
  noise_db: number | null;
  last_event_tick: number | null;
  last_event_id?: number;
  updated_at?: number;
  [k: string]: unknown;
}

export interface KpiRow {
  bucket_start: number;
  active_agents: number;
  total_actions: number;
  total_transits: number;
  chaos_events: number;
  tick_count: number;
  shift_changes: number;
  nightrun_events: number;
  updated_at: number;
  [k: string]: unknown;
}

export interface EventLogRow {
  id: number;
  event_id: string;
  event_type: string;
  aggregate_id: string;
  payload: string;
  correlation_id?: string;
  causation_id?: string | null;
  tick: number;
  timestamp_ms: number;
  compensation_type?: string;
  [k: string]: unknown;
}

interface ConsoleState {
  agents: AgentRow[];
  rooms: RoomRow[];
  kpi: KpiRow | null;
  events: EventLogRow[];
  lastTopic: string | null;
  lastHello: Record<string, unknown> | null;
}

export const EVENT_LOG_CAP = 10_000;

const [state, setState] = createStore<ConsoleState>({
  agents: [],
  rooms: [],
  kpi: null,
  events: [],
  lastTopic: null,
  lastHello: null,
});

const [status, setStatus] = createSignal<ConnectionStatus>("disconnected");
const [frameCount, setFrameCount] = createSignal(0);
// Geteilter Agent-Filter (Control-Center setzt ihn, Dashboard-Liste filtert reaktiv darauf).
const [agentFilter, setAgentFilter] = createSignal("");

function eventKey(row: EventLogRow): string | null {
  if (typeof row.id === "number" && Number.isFinite(row.id)) return `id:${row.id}`;
  if (row.event_id) return `event:${row.event_id}`;
  return null;
}

export function appendEventRows(current: readonly EventLogRow[], rows: EventLogRow[]): EventLogRow[] {
  if (rows.length === 0) return current.slice();
  const byKey = new Map<string, EventLogRow>();
  for (const row of current) {
    const key = eventKey(row);
    if (key) byKey.set(key, row);
  }
  for (const row of rows) {
    const key = eventKey(row);
    if (key) byKey.set(key, row);
  }
  return [...byKey.values()]
    .sort((a, b) => (a.id ?? 0) - (b.id ?? 0))
    .slice(-EVENT_LOG_CAP);
}

/** Verarbeitet einen gepushten topic-Frame in den reaktiven Store. */
export function ingestFrame(topic: string, value: unknown) {
  setState("lastTopic", topic);
  setFrameCount((n) => n + 1);
  if (topic === "hello") {
    setState("lastHello", value as Record<string, unknown>);
  } else if (topic === "agent_live" && value && typeof value === "object") {
    const rows = (value as { agents?: AgentRow[] }).agents;
    if (Array.isArray(rows)) setState("agents", reconcile(rows, { key: "agent_id" }));
  } else if (topic === "room_live" && value && typeof value === "object") {
    const rows = (value as { rooms?: RoomRow[] }).rooms;
    if (Array.isArray(rows)) setState("rooms", reconcile(rows, { key: "room_id" }));
  } else if (topic === "kpi" && value && typeof value === "object") {
    const row = (value as { kpi?: KpiRow | null }).kpi;
    setState("kpi", row ?? null);
  } else if (topic === "event_log" && value && typeof value === "object") {
    const rows = (value as { events?: EventLogRow[] }).events;
    if (Array.isArray(rows)) setState("events", appendEventRows(state.events, rows));
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
export { status, frameCount, setStatus, agentFilter, setAgentFilter };
