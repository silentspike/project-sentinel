// Bun:sqlite read-only Zugriff auf projection.db und EventStore DB.
// Lag-Berechnung: MAX(events.id) - projection_offsets.last_event_id

import { Database } from "bun:sqlite";
import type { AgentRow, RoomRow, KpiRow } from "./types";

let projectionDb: Database;
let eventStoreDb: Database;

export function openDatabases(
  projectionPath: string,
  eventStorePath: string,
): void {
  projectionDb = new Database(projectionPath, { readonly: true });
  eventStoreDb = new Database(eventStorePath, { readonly: true });
}

export function setDatabases(proj: Database, es: Database): void {
  projectionDb = proj;
  eventStoreDb = es;
}

export function closeDatabases(): void {
  projectionDb?.close();
  eventStoreDb?.close();
}

// ── Agent Queries ────────────────────────────────

export function getActiveAgents(): AgentRow[] {
  return projectionDb
    .query<AgentRow, []>(
      "SELECT * FROM agent_live_view WHERE status = 'active' ORDER BY agent_id",
    )
    .all();
}

export function getAllAgents(): AgentRow[] {
  return projectionDb
    .query<AgentRow, []>("SELECT * FROM agent_live_view ORDER BY agent_id")
    .all();
}

export function getAgentById(id: number): AgentRow | null {
  return projectionDb
    .query<AgentRow, [number]>(
      "SELECT * FROM agent_live_view WHERE agent_id = ?",
    )
    .get(id);
}

export function getAgentByName(name: string): AgentRow | null {
  return projectionDb
    .query<AgentRow, [string]>(
      "SELECT * FROM agent_live_view WHERE name = ?",
    )
    .get(name);
}

// ── Room Queries ─────────────────────────────────

export function getAllRooms(): RoomRow[] {
  return projectionDb
    .query<RoomRow, []>("SELECT * FROM room_live_view ORDER BY room_id")
    .all();
}

export function getRoom(roomId: string): RoomRow | null {
  return projectionDb
    .query<RoomRow, [string]>(
      "SELECT * FROM room_live_view WHERE room_id = ?",
    )
    .get(roomId);
}

// ── KPI Queries ──────────────────────────────────

export function getLatestKpi(): KpiRow | null {
  return projectionDb
    .query<KpiRow, []>(
      "SELECT * FROM kpi_1m ORDER BY bucket_start DESC LIMIT 1",
    )
    .get();
}

// ── Lag Berechnung ───────────────────────────────

export function getProjectionLag(): number {
  const maxRow = eventStoreDb
    .query<{ max_id: number | null }, []>(
      "SELECT MAX(id) as max_id FROM events",
    )
    .get();

  const offsetRow = eventStoreDb
    .query<{ last_event_id: number }, [string]>(
      "SELECT last_event_id FROM projection_offsets WHERE projection_name = ?",
    )
    .get("sentinel-projection");

  const maxId = maxRow?.max_id ?? 0;
  const offset = offsetRow?.last_event_id ?? 0;
  return Math.max(0, maxId - offset);
}

// ── Change Detection (fuer WebSocket) ────────────

export function getMaxAgentEventId(): number {
  const row = projectionDb
    .query<{ max_id: number | null }, []>(
      "SELECT MAX(last_event_id) as max_id FROM agent_live_view",
    )
    .get();
  return row?.max_id ?? 0;
}

export function getMaxRoomEventId(): number {
  const row = projectionDb
    .query<{ max_id: number | null }, []>(
      "SELECT MAX(last_event_id) as max_id FROM room_live_view",
    )
    .get();
  return row?.max_id ?? 0;
}
