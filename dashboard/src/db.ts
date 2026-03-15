// Bun:sqlite read-only Zugriff auf projection.db und EventStore DB.
// Lag-Berechnung: MAX(events.id) - projection_offsets.last_event_id

import { Database } from "bun:sqlite";
import type { AgentRow, RoomRow, KpiRow, EventRow, EvolutionRow, ChaosEventItem, ChatMessage } from "./types";

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

export function getEventStoreDb(): Database {
  return eventStoreDb;
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
  // Aggregate KPI across ALL buckets for cumulative counters (chaos_events,
  // shift_changes, nightrun_events), and use latest values for gauges
  // (active_agents, tick_count). Single-bucket query missed sparse events.
  return projectionDb
    .query<KpiRow, []>(
      `SELECT
         MAX(bucket_start) as bucket_start,
         (SELECT active_agents FROM kpi_1m ORDER BY bucket_start DESC LIMIT 1) as active_agents,
         SUM(total_actions) as total_actions,
         SUM(total_transits) as total_transits,
         SUM(chaos_events) as chaos_events,
         (SELECT tick_count FROM kpi_1m ORDER BY bucket_start DESC LIMIT 1) as tick_count,
         SUM(shift_changes) as shift_changes,
         SUM(nightrun_events) as nightrun_events
       FROM kpi_1m`,
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

export function getRecentIncidentEvents(hours: number, limit = 200): EventRow[] {
  const cutoff = Date.now() - hours * 3600_000;
  return eventStoreDb
    .query<EventRow, [number, number]>(
      `SELECT id, event_id, event_type, aggregate_id, payload,
              correlation_id, causation_id, tick, timestamp_ms, compensation_type
       FROM events
       WHERE event_type IN (${INCIDENT_TYPES_SQL})
         AND timestamp_ms > ?
       ORDER BY id DESC
       LIMIT ?`,
    )
    .all(cutoff, limit);
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

// Proximity-based event correlation: finds agent actions in the same room
// within a tick window after a given event (e.g., chaos → agent reactions).
export function getEventsNearby(
  roomId: string,
  afterTick: number,
  windowTicks: number,
  limit = 20,
): EventRow[] {
  return eventStoreDb
    .query<EventRow, [number, number, string, number]>(
      `SELECT id, event_id, event_type, aggregate_id, payload,
              correlation_id, causation_id, tick, timestamp_ms, compensation_type
       FROM events
       WHERE tick BETWEEN ? AND ?
         AND event_type = 'agent_action_received'
         AND payload LIKE ?
       ORDER BY tick ASC
       LIMIT ?`,
    )
    .all(afterTick, afterTick + windowTicks, `%"target_room":"${roomId}"%`, limit);
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

// ── Chaos Event Feed ────────────────────────────

export function getRecentChaosEvents(limit = 100): ChaosEventItem[] {
  return eventStoreDb
    .query<{ id: number; event_id: string; aggregate_id: string; payload: string; tick: number; timestamp_ms: number }, [number]>(
      `SELECT id, event_id, aggregate_id, payload, tick, timestamp_ms
       FROM events
       WHERE event_type = 'chaos_triggered'
       ORDER BY id DESC
       LIMIT ?`,
    )
    .all(limit)
    .map((row) => {
      let chaosType = "unknown";
      let description = "";
      let roomId: string | null = row.aggregate_id;
      try {
        const p = JSON.parse(row.payload);
        chaosType = String(p.event_type ?? "unknown");
        description = String(p.description ?? "");
        if (p.target_room) roomId = String(p.target_room);
      } catch { /* ignore parse errors */ }
      return {
        id: row.id,
        event_id: row.event_id,
        chaos_type: chaosType,
        room_id: roomId,
        description,
        tick: row.tick,
        timestamp_ms: row.timestamp_ms,
      };
    });
}

export function getChaosEventsByRoom(roomId: string, limit = 50): ChaosEventItem[] {
  return eventStoreDb
    .query<{ id: number; event_id: string; aggregate_id: string; payload: string; tick: number; timestamp_ms: number }, [string, string, number]>(
      `SELECT id, event_id, aggregate_id, payload, tick, timestamp_ms
       FROM events
       WHERE event_type = 'chaos_triggered'
         AND (aggregate_id = ? OR payload LIKE ?)
       ORDER BY id DESC
       LIMIT ?`,
    )
    .all(roomId, `%"target_room":"${roomId}"%`, limit)
    .map((row) => {
      let chaosType = "unknown";
      let description = "";
      try {
        const p = JSON.parse(row.payload);
        chaosType = String(p.event_type ?? "unknown");
        description = String(p.description ?? "");
      } catch { /* ignore */ }
      return {
        id: row.id,
        event_id: row.event_id,
        chaos_type: chaosType,
        room_id: roomId,
        description,
        tick: row.tick,
        timestamp_ms: row.timestamp_ms,
      };
    });
}

export function getMaxChaosEventId(): number {
  const row = eventStoreDb
    .query<{ max_id: number | null }, []>(
      "SELECT MAX(id) as max_id FROM events WHERE event_type = 'chaos_triggered'",
    )
    .get();
  return row?.max_id ?? 0;
}

// ── Chat Messages (Agent Actions) ───────────────

export function getRecentChatMessages(limit = 100): ChatMessage[] {
  return eventStoreDb
    .query<{ id: number; event_id: string; aggregate_id: string; payload: string; tick: number; timestamp_ms: number }, [number]>(
      `SELECT id, event_id, aggregate_id, payload, tick, timestamp_ms
       FROM events
       WHERE event_type = 'agent_action_received'
       ORDER BY id DESC
       LIMIT ?`,
    )
    .all(limit)
    .map(toChatMessage);
}

export function getChatMessagesByRoom(roomId: string, limit = 50): ChatMessage[] {
  return eventStoreDb
    .query<{ id: number; event_id: string; aggregate_id: string; payload: string; tick: number; timestamp_ms: number }, [string, number]>(
      `SELECT id, event_id, aggregate_id, payload, tick, timestamp_ms
       FROM events
       WHERE event_type = 'agent_action_received'
         AND payload LIKE ?
       ORDER BY id DESC
       LIMIT ?`,
    )
    .all(`%"target_room":"${roomId}"%`, limit)
    .map(toChatMessage);
}

function toChatMessage(row: { id: number; event_id: string; aggregate_id: string; payload: string; tick: number; timestamp_ms: number }): ChatMessage {
  let agentId = row.aggregate_id;
  let agentName = row.aggregate_id;
  let actionType = "";
  let content: string | null = null;
  let targetRoom: string | null = null;
  try {
    const p = JSON.parse(row.payload);
    if (p.agent_id) agentId = String(typeof p.agent_id === "object" ? p.agent_id[0] ?? p.agent_id : p.agent_id);
    agentName = agentId;
    actionType = String(p.action_type ?? "");
    content = p.content ? String(p.content) : null;
    targetRoom = p.target_room ? String(p.target_room) : null;
  } catch { /* ignore */ }
  return {
    id: row.id,
    event_id: row.event_id,
    agent_id: agentId,
    agent_name: agentName,
    action_type: actionType,
    content,
    target_room: targetRoom,
    tick: row.tick,
    timestamp_ms: row.timestamp_ms,
  };
}

// ── Room Occupants ──────────────────────────────

export function getOccupantsByRoom(): Record<string, string[]> {
  const rows = projectionDb
    .query<{ name: string; current_room: string }, []>(
      "SELECT name, current_room FROM agent_live_view WHERE status = 'active' AND current_room IS NOT NULL",
    )
    .all();
  const result: Record<string, string[]> = {};
  for (const row of rows) {
    if (!result[row.current_room]) result[row.current_room] = [];
    result[row.current_room].push(row.name);
  }
  return result;
}

// ── Activity Feed (EventStore Timeline) ─────────

/** Event-Typen die im Activity-Feed angezeigt werden (ohne bio_state_updated + tick_snapshot = zu hochfrequent) */
const ACTIVITY_EVENT_TYPES = [
  "agent_spawned",
  "agent_despawned",
  "agent_action_received",
  "agent_status_changed",
  "transit_started",
  "transit_completed",
  "chaos_triggered",
  "bio_action_performed",
  "bio_state_updated",
  "room_physics_updated",
  "shift_transition_completed",
  "nightrun_started",
  "nightrun_completed",
  "agent_consolidated",
  "agent_consolidation_failed",
] as const;

const ACTIVITY_TYPES_SQL = ACTIVITY_EVENT_TYPES.map((t) => `'${t}'`).join(",");

export function getRecentActivityEvents(limit = 200): EventRow[] {
  return eventStoreDb
    .query<EventRow, [number]>(
      `SELECT id, event_id, event_type, aggregate_id, payload,
              correlation_id, causation_id, tick, timestamp_ms, compensation_type
       FROM events
       WHERE event_type IN (${ACTIVITY_TYPES_SQL})
       ORDER BY id DESC
       LIMIT ?`,
    )
    .all(limit);
}

export function getMaxActivityEventId(): number {
  const row = eventStoreDb
    .query<{ max_id: number | null }, []>(
      `SELECT MAX(id) as max_id FROM events
       WHERE event_type IN (${ACTIVITY_TYPES_SQL})`,
    )
    .get();
  return row?.max_id ?? 0;
}

// ── Total Event Count ───────────────────────────

export function getTotalEventCount(): number {
  const row = eventStoreDb
    .query<{ cnt: number }, []>("SELECT COUNT(*) as cnt FROM events")
    .get();
  return row?.cnt ?? 0;
}

export function getEventRatePerMinute(): number {
  const fiveMinAgo = Date.now() - 5 * 60_000;
  const row = eventStoreDb
    .query<{ cnt: number }, [number]>(
      "SELECT COUNT(*) as cnt FROM events WHERE timestamp_ms > ?",
    )
    .get(fiveMinAgo);
  return Math.round(((row?.cnt ?? 0) / 5) * 10) / 10;
}
