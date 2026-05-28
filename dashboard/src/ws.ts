// WebSocket-Handler mit DB-Poll Change-Detection.
// Pollt agent_live_view + room_live_view auf Aenderungen via MAX(last_event_id).
// Bei Aenderung: Broadcast Snapshot an alle verbundenen Clients.

import type { ServerWebSocket } from "bun";
import {
  getActiveAgents,
  getAllRooms,
  getGlobalMaxEventId,
  getOccupantsByRoom,
  getProjectionLag,
} from "./db";
import { ROOM_METADATA } from "./rooms-meta";
import type { AgentListItem, RoomResponse, RoomRow } from "./types";

const clients = new Set<ServerWebSocket<unknown>>();

let lastGlobalEventId = 0;
let pollTimer: ReturnType<typeof setInterval> | null = null;
let healthTimer: ReturnType<typeof setInterval> | null = null;

const startTime = Date.now();

function toAgentListItem(row: {
  agent_id: number;
  name: string;
  role: string;
  status: string;
  current_room: string | null;
  in_transit: number;
  transit_target: string | null;
  last_action: string | null;
  last_action_tick: number | null;
  hunger: number | null;
  energy: number | null;
  stress: number | null;
  bladder: number | null;
  social_need: number | null;
  caffeine_mg: number | null;
  mood: string | null;
}): AgentListItem {
  const meta = row.current_room ? ROOM_METADATA[row.current_room] : null;
  return {
    id: row.agent_id,
    name: row.name,
    role: row.role,
    status: row.status,
    current_room: row.current_room,
    room_name: meta?.name ?? null,
    in_transit: row.in_transit !== 0,
    transit_target: row.transit_target,
    last_action: row.last_action,
    last_action_tick: row.last_action_tick,
    hunger: row.hunger ?? 0,
    energy: row.energy ?? 1,
    stress: row.stress ?? 0,
    bladder: row.bladder ?? 0,
    social_need: row.social_need ?? 0,
    caffeine_mg: row.caffeine_mg ?? 0,
    mood: row.mood ?? null,
    stalled: false, // WebSocket updates don't include stall data (polled via REST)
  };
}

function toRoomResponse(row: RoomRow, occupants: string[]): RoomResponse {
  const meta = ROOM_METADATA[row.room_id];
  let chaos: unknown | null = null;
  if (row.active_chaos) {
    try {
      chaos = JSON.parse(row.active_chaos);
    } catch {
      chaos = null;
    }
  }
  let smells: unknown | null = null;
  if (row.active_smells) {
    try {
      smells = JSON.parse(row.active_smells);
    } catch {
      smells = null;
    }
  }
  return {
    id: row.room_id,
    name: meta?.name ?? row.room_id,
    floor: meta?.floor ?? 0,
    capacity: meta?.capacity ?? 0,
    room_type: meta?.room_type ?? "unknown",
    occupant_count: row.occupant_count,
    transit_count: row.transit_count,
    active_chaos: chaos,
    active_smells: smells,
    temperature: row.temperature,
    co2_ppm: row.co2_ppm,
    noise_db: row.noise_db,
    last_event_tick: row.last_event_tick,
    occupants,
  };
}

export function broadcast(data: unknown): void {
  const msg = JSON.stringify(data);
  for (const ws of clients) {
    try {
      ws.send(msg);
    } catch {
      clients.delete(ws);
    }
  }
}

export function pollForChanges(): void {
  try {
    const currentMax = getGlobalMaxEventId();
    if (currentMax <= lastGlobalEventId) return;

    const agents = getActiveAgents();
    broadcast({ type: "agent_update", agents: agents.map(toAgentListItem) });

    const rooms = getAllRooms();
    const occupantsMap = getOccupantsByRoom();
    broadcast({ type: "room_update", rooms: rooms.map((r) => toRoomResponse(r, occupantsMap[r.room_id] ?? [])) });

    broadcast({ type: "cockpit_update" });
    broadcast({ type: "chaos_update" });
    broadcast({ type: "activity_update" });

    lastGlobalEventId = currentMax;
  } catch {
    // DB-Fehler beim Poll — skip, naechster Versuch in 1s
  }
}

function sendHealthUpdate(): void {
  let lag = 0;
  try {
    lag = getProjectionLag();
  } catch {
    // EventStore nicht verfuegbar
  }
  broadcast({
    type: "health_update",
    lag,
    uptime: Math.floor((Date.now() - startTime) / 1000),
  });
}

export function resetWatermarks(): void {
  lastGlobalEventId = 0;
}

export function startPolling(): void {
  const pollMs = parseInt(process.env.WS_POLL_INTERVAL_MS || "1000", 10);
  pollTimer = setInterval(pollForChanges, pollMs);
  healthTimer = setInterval(sendHealthUpdate, 5000);
}

export function stopPolling(): void {
  if (pollTimer) clearInterval(pollTimer);
  if (healthTimer) clearInterval(healthTimer);
  pollTimer = null;
  healthTimer = null;
}

export function createWsHandler() {
  return {
    open(ws: ServerWebSocket<unknown>) {
      clients.add(ws);
    },
    message(_ws: ServerWebSocket<unknown>, _message: string | Buffer) {
      // Client-Nachrichten ignoriert (read-only Dashboard)
    },
    close(ws: ServerWebSocket<unknown>) {
      clients.delete(ws);
    },
  };
}

export function getClientCount(): number {
  return clients.size;
}
