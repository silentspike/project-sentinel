// Bun:sqlite read-only Zugriff auf projection.db und EventStore DB.
// Lag-Berechnung: MAX(events.id) - projection_offsets.last_event_id

import { Database } from "bun:sqlite";
import type { AgentRow, RoomRow, KpiRow, EventRow, EvolutionRow } from "./types";

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

// ── Cockpit Queries ───────────────────────────────

const INCIDENT_EVENT_TYPES = [
  "chaos_triggered",
  "agent_consolidation_failed",
  "agent_despawned",
  "nightrun_completed",
] as const;

const INCIDENT_TYPES_SQL = INCIDENT_EVENT_TYPES.map((t) => `'${t}'`).join(",");

export function getRecentIncidentEvents(hours: number): EventRow[] {
  const cutoff = Date.now() - hours * 3600_000;
  return eventStoreDb
    .query<EventRow, [number]>(
      `SELECT id, event_id, event_type, aggregate_id, payload,
              correlation_id, causation_id, tick, timestamp_ms, compensation_type
       FROM events
       WHERE event_type IN (${INCIDENT_TYPES_SQL})
         AND timestamp_ms > ?
       ORDER BY id DESC`,
    )
    .all(cutoff);
}

export function getRecentEvolutionAlerts(hours: number): EvolutionRow[] {
  const cutoff = Date.now() - hours * 3600_000;
  try {
    return eventStoreDb
      .query<EvolutionRow, [number]>(
        `SELECT id, agent_id, tick, field, change_type, old_value,
                new_value, reason, nmda_score, source, created_at_ms
         FROM personality_evolution
         WHERE change_type IN ('drift', 'fatigue_spike', 'quality_shift')
           AND created_at_ms > ?
         ORDER BY id DESC`,
      )
      .all(cutoff);
  } catch {
    // personality_evolution table may not exist if judge never ran
    return [];
  }
}

export function getEventsByCorrelation(correlationId: string): EventRow[] {
  return eventStoreDb
    .query<EventRow, [string]>(
      `SELECT id, event_id, event_type, aggregate_id, payload,
              correlation_id, causation_id, tick, timestamp_ms, compensation_type
       FROM events
       WHERE correlation_id = ?
       ORDER BY id ASC`,
    )
    .all(correlationId);
}

export function getEventsByCausation(eventId: string): EventRow[] {
  return eventStoreDb
    .query<EventRow, [string]>(
      `SELECT id, event_id, event_type, aggregate_id, payload,
              correlation_id, causation_id, tick, timestamp_ms, compensation_type
       FROM events
       WHERE causation_id = ?
       ORDER BY id ASC`,
    )
    .all(eventId);
}

export function getEventById(eventId: string): EventRow | null {
  return eventStoreDb
    .query<EventRow, [string]>(
      `SELECT id, event_id, event_type, aggregate_id, payload,
              correlation_id, causation_id, tick, timestamp_ms, compensation_type
       FROM events
       WHERE event_id = ?`,
    )
    .get(eventId) ?? null;
}

export function getChaosCountLastHour(): number {
  const cutoff = Date.now() - 3600_000;
  const row = eventStoreDb
    .query<{ cnt: number }, [number]>(
      `SELECT COUNT(*) as cnt FROM events
       WHERE event_type = 'chaos_triggered' AND timestamp_ms > ?`,
    )
    .get(cutoff);
  return row?.cnt ?? 0;
}

export function getUnexpectedDespawnCount(): number {
  const cutoff = Date.now() - 3600_000;
  const row = eventStoreDb
    .query<{ cnt: number }, [number]>(
      `SELECT COUNT(*) as cnt FROM events
       WHERE event_type = 'agent_despawned'
         AND payload NOT LIKE '%"reason":"shift"%'
         AND timestamp_ms > ?`,
    )
    .get(cutoff);
  return row?.cnt ?? 0;
}

export function getLastNightrunStats(): {
  consolidated: number;
  failed: number;
} | null {
  const row = eventStoreDb
    .query<{ payload: string }, []>(
      `SELECT payload FROM events
       WHERE event_type = 'nightrun_completed'
       ORDER BY id DESC LIMIT 1`,
    )
    .get();
  if (!row) return null;
  try {
    const p = JSON.parse(row.payload);
    return {
      consolidated: p.agents_consolidated ?? 0,
      failed: p.agents_failed ?? 0,
    };
  } catch {
    return null;
  }
}

export function getMaxIncidentEventId(): number {
  const row = eventStoreDb
    .query<{ max_id: number | null }, []>(
      `SELECT MAX(id) as max_id FROM events
       WHERE event_type IN (${INCIDENT_TYPES_SQL})`,
    )
    .get();
  return row?.max_id ?? 0;
}
