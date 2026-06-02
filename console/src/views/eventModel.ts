import type { AgentRow, EventLogRow } from "../stores/console";
import { ROOM_METADATA, roomDisplayName } from "../roomsMeta";
import { formatDateTime } from "./format";

export const EVENT_TYPE_LABELS: Record<string, string> = {
  agent_spawned: "Spawn",
  agent_despawned: "Despawn",
  agent_action_received: "Aktion",
  agent_status_changed: "Status",
  transit_started: "Transit",
  transit_completed: "Ankunft",
  chaos_triggered: "Chaos",
  bio_action_performed: "Bio",
  bio_state_updated: "Bio",
  room_physics_updated: "Physik",
  room_stimulus_applied: "Stimulus",
  shift_transition_completed: "Schicht",
  nightrun_started: "Nightrun",
  nightrun_completed: "Nightrun",
  agent_consolidated: "Memory",
  agent_consolidation_failed: "Memory",
};

export const EVENT_TYPE_TONE: Record<string, string> = {
  agent_spawned: "lifecycle",
  agent_despawned: "lifecycle",
  agent_action_received: "action",
  agent_status_changed: "system",
  transit_started: "transit",
  transit_completed: "transit",
  chaos_triggered: "chaos",
  bio_action_performed: "bio",
  bio_state_updated: "bio",
  room_physics_updated: "physics",
  room_stimulus_applied: "physics",
  shift_transition_completed: "system",
  nightrun_started: "memory",
  nightrun_completed: "memory",
  agent_consolidated: "memory",
  agent_consolidation_failed: "chaos",
};

export const ACTIVITY_FILTERS = [
  { key: "all", label: "Alle", types: null },
  {
    key: "focus",
    label: "Reaktionen",
    types: ["agent_action_received", "chaos_triggered", "transit_started", "transit_completed", "bio_action_performed"],
  },
  { key: "actions", label: "Aktionen", types: ["agent_action_received"] },
  { key: "chaos", label: "Chaos", types: ["chaos_triggered"] },
  { key: "transit", label: "Transit", types: ["transit_started", "transit_completed"] },
  { key: "physics", label: "Physik", types: ["room_physics_updated"] },
  { key: "bio", label: "Bio", types: ["bio_state_updated", "bio_action_performed"] },
] as const;

export type ActivityFilterKey = (typeof ACTIVITY_FILTERS)[number]["key"];

export interface ActivityItem {
  event: EventLogRow;
  summary: string;
  detail: string | null;
  room: string | null;
  badge: string;
  tone: string;
  timestamp: string;
  searchText: string;
}

export interface ChatMessage {
  id: number;
  event_id: string;
  agent_id: string;
  agent_name: string;
  action_type: string;
  content: string;
  target_room: string | null;
  tick: number;
  timestamp_ms: number;
}

export function payloadOf(event: EventLogRow): Record<string, unknown> {
  if (typeof event.payload !== "string" || event.payload.length === 0) return {};
  try {
    const parsed = JSON.parse(event.payload) as unknown;
    return parsed && typeof parsed === "object" && !Array.isArray(parsed) ? parsed as Record<string, unknown> : {};
  } catch {
    return {};
  }
}

function stringValue(value: unknown): string | null {
  if (typeof value === "string" && value.length > 0) return value;
  if (typeof value === "number" && Number.isFinite(value)) return String(value);
  if (Array.isArray(value) && value.length > 0) return stringValue(value[0]);
  return null;
}

function numberValue(value: unknown): number | null {
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}

export function eventRoom(event: EventLogRow): string | null {
  const payload = payloadOf(event);
  return stringValue(payload.target_room)
    ?? stringValue(payload.room_id)
    ?? (ROOM_METADATA[event.aggregate_id] ? event.aggregate_id : null);
}

function agentId(event: EventLogRow): string {
  return stringValue(payloadOf(event).agent_id) ?? event.aggregate_id;
}

function agentName(event: EventLogRow, agents: readonly AgentRow[]): string {
  const payload = payloadOf(event);
  const id = agentId(event);
  const fromPayload = stringValue(payload.name) ?? stringValue(payload.agent_name);
  if (fromPayload) return fromPayload;
  const known = agents.find((agent) => String(agent.agent_id) === id || agent.name === id);
  return known?.name ?? id;
}

export function chaosType(event: EventLogRow): string {
  const payload = payloadOf(event);
  return stringValue(payload.event_type) ?? stringValue(payload.chaos_type) ?? "unknown";
}

export function activityItem(event: EventLogRow, agents: readonly AgentRow[] = []): ActivityItem {
  const payload = payloadOf(event);
  const name = agentName(event, agents);
  let summary = event.event_type;
  let detail: string | null = null;
  let room = eventRoom(event);

  switch (event.event_type) {
    case "agent_spawned":
      summary = `${stringValue(payload.name) ?? name} gespawnt`;
      detail = stringValue(payload.role) ? `Rolle: ${stringValue(payload.role)}` : null;
      break;
    case "agent_despawned":
      summary = `${name} despawnt`;
      detail = stringValue(payload.reason) ? `Grund: ${stringValue(payload.reason)}` : null;
      break;
    case "agent_action_received":
      summary = stringValue(payload.content)
        ? `${name}: ${stringValue(payload.content)}`
        : `${name}: ${stringValue(payload.action_type) ?? "Aktion"}`;
      detail = stringValue(payload.action_type);
      break;
    case "transit_started":
      summary = `${name} unterwegs`;
      detail = `${stringValue(payload.from_room) ?? "?"} -> ${stringValue(payload.to_room) ?? "?"}`;
      room = stringValue(payload.to_room) ?? room;
      break;
    case "transit_completed":
      summary = `${name} angekommen`;
      room = stringValue(payload.room_id) ?? room;
      detail = room ? `in ${roomDisplayName(room)}` : null;
      break;
    case "chaos_triggered":
      summary = `Chaos: ${chaosType(event)}`;
      detail = stringValue(payload.description);
      room = stringValue(payload.target_room) ?? room ?? event.aggregate_id;
      break;
    case "bio_action_performed":
      summary = `${name}: ${stringValue(payload.action) ?? "Bio-Aktion"}`;
      detail = stringValue(payload.description);
      break;
    case "bio_state_updated": {
      summary = `${name} Bio-Update`;
      const hunger = numberValue(payload.hunger);
      const energy = numberValue(payload.energy);
      const stress = numberValue(payload.stress);
      detail = hunger != null && energy != null && stress != null
        ? `H:${(hunger * 100).toFixed(0)}% E:${(energy * 100).toFixed(0)}% S:${(stress * 100).toFixed(0)}%`
        : null;
      break;
    }
    case "room_physics_updated": {
      const temp = numberValue(payload.temperature);
      const co2 = numberValue(payload.co2_ppm);
      room = stringValue(payload.room_id) ?? room ?? event.aggregate_id;
      summary = `Raum ${roomDisplayName(room)} Physik`;
      detail = `${temp != null ? temp.toFixed(1) : "?"} C CO2:${co2 != null ? co2.toFixed(0) : "?"}ppm`;
      break;
    }
    case "shift_transition_completed":
      summary = "Schichtwechsel";
      detail = Array.isArray(payload.removed_agents) ? `${payload.removed_agents.length} Agents entfernt` : null;
      break;
    case "nightrun_started":
      summary = "Nightrun gestartet";
      break;
    case "nightrun_completed":
      summary = "Nightrun abgeschlossen";
      detail = `${numberValue(payload.agents_consolidated) ?? 0} konsolidiert, ${numberValue(payload.agents_failed) ?? 0} fehlgeschlagen`;
      break;
    case "agent_consolidated":
      summary = `${name} konsolidiert`;
      detail = Array.isArray(payload.episodes) ? `${payload.episodes.length} Episoden` : null;
      break;
    case "agent_consolidation_failed":
      summary = `${name} Konsolidierung fehlgeschlagen`;
      detail = stringValue(payload.error);
      break;
    case "agent_status_changed":
      summary = `${name} Status: ${stringValue(payload.new_status) ?? "?"}`;
      break;
  }

  const label = EVENT_TYPE_LABELS[event.event_type] ?? event.event_type;
  const badge = event.event_type === "chaos_triggered" ? chaosType(event) : label;
  const timestamp = formatDateTime(event.timestamp_ms);
  const haystack = [summary, detail, room, roomDisplayName(room), label, event.event_type, event.aggregate_id]
    .filter(Boolean)
    .join(" ")
    .toLowerCase();

  return {
    event,
    summary,
    detail,
    room,
    badge,
    tone: EVENT_TYPE_TONE[event.event_type] ?? "system",
    timestamp,
    searchText: haystack,
  };
}

export function toChatMessage(event: EventLogRow, agents: readonly AgentRow[] = []): ChatMessage | null {
  if (event.event_type !== "agent_action_received") return null;
  const payload = payloadOf(event);
  const content = stringValue(payload.content);
  if (!content) return null;
  return {
    id: event.id,
    event_id: event.event_id,
    agent_id: agentId(event),
    agent_name: agentName(event, agents),
    action_type: stringValue(payload.action_type) ?? "",
    content,
    target_room: stringValue(payload.target_room),
    tick: event.tick,
    timestamp_ms: event.timestamp_ms,
  };
}
