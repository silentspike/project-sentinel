// WebSocket-Handler mit DB-Poll Change-Detection.
// Pollt agent_live_view + room_live_view auf Aenderungen via MAX(last_event_id).
// Bei Aenderung: Broadcast Snapshot an alle verbundenen Clients.

import type { ServerWebSocket } from "bun";
import {
  getActiveAgents,
  getAllRooms,
  getMaxAgentEventId,
  getMaxRoomEventId,
  getMaxIncidentEventId,
  getMaxChaosEventId,
  getOccupantsByRoom,
  getProjectionLag,
} from "./db";
import { ROOM_METADATA } from "./rooms-meta";
import type { AgentListItem, RoomResponse, RoomRow } from "./types";

const clients = new Set<ServerWebSocket<unknown>>();

let lastAgentEventId = 0;
let lastRoomEventId = 0;
let lastIncidentEventId = 0;
let lastChaosEventId = 0;
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
  return {
    id: row.room_id,
    name: meta?.name ?? row.room_id,
    floor: meta?.floor ?? 0,
    capacity: meta?.capacity ?? 0,
    room_type: meta?.room_type ?? "unknown",
    occupant_count: row.occupant_count,
    transit_count: row.transit_count,
    active_chaos: chaos,
    last_event_tick: row.last_event_tick,
    occupants,
  };
}

function broadcast(data: unknown): void {
  const msg = JSON.stringify(data);
  for (const ws of clients) {
    try {
      ws.send(msg);
    } catch {
      clients.delete(ws);
    }
  }
}

function pollForChanges(): void {
  try {
    const currentAgentMax = getMaxAgentEventId();
    if (currentAgentMax > lastAgentEventId) {
      const agents = getActiveAgents();
      broadcast({ type: "agent_update", agents: agents.map(toAgentListItem) });
      lastAgentEventId = currentAgentMax;
    }

    const currentRoomMax = getMaxRoomEventId();
    if (currentRoomMax > lastRoomEventId) {
      const rooms = getAllRooms();
      const occupantsMap = getOccupantsByRoom();
      broadcast({ type: "room_update", rooms: rooms.map((r) => toRoomResponse(r, occupantsMap[r.room_id] ?? [])) });
      lastRoomEventId = currentRoomMax;
    }

    const currentIncidentMax = getMaxIncidentEventId();
    if (currentIncidentMax > lastIncidentEventId) {
      broadcast({ type: "cockpit_update" });
      lastIncidentEventId = currentIncidentMax;
    }

    const currentChaosMax = getMaxChaosEventId();
    if (currentChaosMax > lastChaosEventId) {
      broadcast({ type: "chaos_update" });
      lastChaosEventId = currentChaosMax;
    }
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
