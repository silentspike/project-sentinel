// Bun:sqlite read-only Zugriff auf projection.db und EventStore DB.
// Lag-Berechnung: MAX(events.id) - projection_offsets.last_event_id

import { Database } from "bun:sqlite";
import type {
  AgentRow,
  RoomRow,
  KpiRow,
  EventRow,
  EvolutionRow,
  ChaosEventItem,
  ChatMessage,
  PlatformAnalysisItem,
  RoomPhysicsHistoryPoint,
  RoomReactionItem,
  RoomStimulusHistoryItem,
} from "./types";

let projectionDb: Database;
let eventStoreDb: Database;
export const ROOM_REACTION_WINDOW_TICKS = 60;
let _eventColumnsCache: Set<string> | null = null;

interface ProjectedRoomOccupants {
  [roomId: string]: {
    agentIds: number[];
    names: string[];
  };
}

interface StoredEventRow {
  event_id: string;
  aggregate_id: string;
  payload: string;
  correlation_id: string;
  tick: number;
  timestamp_ms: number;
}

export function resetCaches(): void {
  _eventColumnsCache = null;
  _agentNameCache = null;
  _agentNameCacheTime = 0;
}

export function openDatabases(
  projectionPath: string,
  eventStorePath: string,
): void {
  projectionDb = new Database(projectionPath, { readonly: true });
  eventStoreDb = new Database(eventStorePath, { readonly: true });
  resetCaches();
}

export function setDatabases(proj: Database, es: Database): void {
  projectionDb = proj;
  eventStoreDb = es;
  resetCaches();
}

export function getEventStoreDb(): Database {
  return eventStoreDb;
}

export function closeDatabases(): void {
  projectionDb?.close();
  eventStoreDb?.close();
  resetCaches();
}

function getExplicitActiveAgents(): AgentRow[] {
  return projectionDb
    .query<AgentRow, []>(
      "SELECT * FROM agent_live_view WHERE status = 'active' ORDER BY agent_id",
    )
    .all();
}

function getProjectedLiveAgents(): AgentRow[] {
  const explicitActive = getExplicitActiveAgents();
  if (explicitActive.length > 0) return explicitActive;

  const occupancyLimitByRoom = new Map(
    getAllRooms().map((room) => [room.room_id, Math.max(0, room.occupant_count)]),
  );
  const selectedByRoom = new Map<string, number>();
  const projectedRows = projectionDb
    .query<AgentRow, []>(
      `SELECT *
       FROM agent_live_view
       WHERE current_room IS NOT NULL
       ORDER BY last_event_id DESC, updated_at DESC, agent_id ASC`,
    )
    .all();

  const liveAgents: AgentRow[] = [];
  for (const row of projectedRows) {
    const roomId = row.current_room;
    if (!roomId) continue;
    const limit = occupancyLimitByRoom.get(roomId);
    if (limit == null || limit <= 0) continue;
    const selected = selectedByRoom.get(roomId) ?? 0;
    if (selected >= limit) continue;
    selectedByRoom.set(roomId, selected + 1);
    liveAgents.push(row.status === "active" ? row : { ...row, status: "active" });
  }

  return liveAgents.sort((a, b) => a.agent_id - b.agent_id);
}

function getProjectedOccupantsByRoom(): ProjectedRoomOccupants {
  const result: ProjectedRoomOccupants = {};
  for (const agent of getProjectedLiveAgents()) {
    if (!agent.current_room) continue;
    if (!result[agent.current_room]) {
      result[agent.current_room] = { agentIds: [], names: [] };
    }
    result[agent.current_room].agentIds.push(agent.agent_id);
    result[agent.current_room].names.push(agent.name);
  }
  return result;
}

function normalizeAgentId(rawAgentId: unknown, fallback: string): {
  agentId: string;
  numericId: number | null;
} {
  const agentId =
    typeof rawAgentId === "object" && Array.isArray(rawAgentId)
      ? String(rawAgentId[0] ?? fallback)
      : String(rawAgentId ?? fallback);
  const match = agentId.match(/(\d+)/);
  return {
    agentId,
    numericId: match ? parseInt(match[1], 10) : null,
  };
}

function getEventColumnSet(): Set<string> {
  if (_eventColumnsCache) {
    return _eventColumnsCache;
  }

  try {
    const rows = eventStoreDb
      .query<{ name: string }, []>("PRAGMA table_info(events)")
      .all();
    _eventColumnsCache = new Set(rows.map((row) => String(row.name)));
  } catch {
    _eventColumnsCache = new Set();
  }
  return _eventColumnsCache;
}

export function eventRowSelectColumns(): string {
  const base =
    "id, event_id, event_type, aggregate_id, payload, correlation_id, causation_id, tick, timestamp_ms";
  if (getEventColumnSet().has("compensation_type")) {
    return `${base}, compensation_type`;
  }
  return `${base}, 'none' AS compensation_type`;
}

// ── Agent Queries ────────────────────────────────

export function getActiveAgents(): AgentRow[] {
  return getProjectedLiveAgents();
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

export function getRoomPhysicsHistory(
  roomId: string,
  limit = 30,
): RoomPhysicsHistoryPoint[] {
  const rows = eventStoreDb
    .query<
      {
        payload: string;
        tick: number;
        timestamp_ms: number;
      },
      [string, number]
    >(
      `SELECT payload, tick, timestamp_ms
       FROM events
       WHERE event_type = 'room_physics_updated'
         AND aggregate_id = ?
       ORDER BY id DESC
       LIMIT ?`,
    )
    .all(roomId, limit);

  return rows
    .map((row) => {
      try {
        const payload = JSON.parse(row.payload) as {
          temperature?: number | null;
          co2_ppm?: number | null;
          noise_db?: number | null;
          occupant_count?: number;
        };
        return {
          tick: row.tick,
          timestamp_ms: row.timestamp_ms,
          temperature: payload.temperature ?? null,
          co2_ppm: payload.co2_ppm ?? null,
          noise_db: payload.noise_db ?? null,
          occupant_count: payload.occupant_count ?? 0,
        };
      } catch {
        return null;
      }
    })
    .filter((row): row is RoomPhysicsHistoryPoint => row !== null)
    .reverse();
}

export function getRoomRecentReactions(
  roomId: string,
  limit = 20,
): RoomReactionItem[] {
  const room = getRoom(roomId);
  const roomLastTick = room?.last_event_tick ?? null;
  const chaosEventsAsc = getChaosEventsByRoom(roomId, 100)
    .slice()
    .sort((a, b) => a.tick - b.tick)
    .filter((event) => roomLastTick == null || event.tick <= roomLastTick);
  const stimulusEventsAsc = getRoomStimulusEventsByRoom(roomId, 100)
    .slice()
    .sort((a, b) => a.tick - b.tick)
    .filter((event) => roomLastTick == null || event.tick <= roomLastTick);
  const occupantsByRoom = getProjectedOccupantsByRoom();
  const occupantIds = new Set(occupantsByRoom[roomId]?.agentIds ?? []);
  const knownRoomIds = new Set(getAllRooms().map((room) => room.room_id));
  const recentTickFloor = Math.max(
    0,
    (roomLastTick ?? ROOM_REACTION_WINDOW_TICKS) - ROOM_REACTION_WINDOW_TICKS,
  );
  const latestStimulus = stimulusEventsAsc.at(-1) ?? null;
  const latestChaos = chaosEventsAsc.at(-1) ?? null;
  const latestContextTick = Math.max(latestStimulus?.tick ?? -1, latestChaos?.tick ?? -1);

  if (latestContextTick >= 0) {
    const windowStartTick = latestContextTick;
    const windowEndTick = latestContextTick + ROOM_REACTION_WINDOW_TICKS;
    const transitRows = eventStoreDb
      .query<StoredEventRow, [number, number, string, number]>(
        `SELECT event_id, aggregate_id, payload, correlation_id, tick, timestamp_ms
         FROM events
         WHERE event_type = 'transit_started'
           AND tick BETWEEN ? AND ?
           AND payload LIKE ?
         ORDER BY id DESC
         LIMIT ?`,
      )
      .all(windowStartTick, windowEndTick, `%"from_room":"${roomId}"%`, limit * 50);
    const transitAgentIds = new Set<number>();
    for (const row of transitRows) {
      const { numericId } = normalizeAgentId(undefined, row.aggregate_id);
      if (numericId != null) transitAgentIds.add(numericId);
    }
    const candidateAgentIds = new Set<number>([...occupantIds, ...transitAgentIds]);
    const windowActions = eventStoreDb
      .query<StoredEventRow, [number, number, number]>(
        `SELECT event_id, aggregate_id, payload, correlation_id, tick, timestamp_ms
         FROM events
         WHERE event_type = 'agent_action_received'
           AND tick BETWEEN ? AND ?
         ORDER BY id DESC
         LIMIT ?`,
      )
      .all(windowStartTick, windowEndTick, limit * 100);
    const correlatedReactions = [
      ...windowActions
        .map((row) =>
          toRoomReactionItem(row, {
            roomId,
            occupantIds: candidateAgentIds,
            knownRoomIds,
            chaosEventsAsc,
            stimulusEventsAsc,
            allowAnyCandidateTarget: true,
          }),
        )
        .filter((row): row is RoomReactionItem => row !== null),
      ...transitRows
        .map((row) =>
          toTransitReactionItem(row, {
            roomId,
            latestChaos,
            latestStimulus,
          }),
        )
        .filter((row): row is RoomReactionItem => row !== null),
    ];
    if (correlatedReactions.length > 0) {
      return finalizeRoomReactions(correlatedReactions, limit);
    }
  }

  const fallbackReactions = eventStoreDb
    .query<StoredEventRow, [number, number]>(
      `SELECT event_id, aggregate_id, payload, correlation_id, tick, timestamp_ms
       FROM events
       WHERE event_type = 'agent_action_received'
         AND tick >= ?
       ORDER BY id DESC
       LIMIT ?`,
    )
    .all(recentTickFloor, limit * 25)
    .map((row) =>
      toRoomReactionItem(row, {
        roomId,
        occupantIds,
        knownRoomIds,
        chaosEventsAsc,
        stimulusEventsAsc,
      }),
    )
    .filter((row): row is RoomReactionItem => row !== null);

  return finalizeRoomReactions(fallbackReactions, limit);
}

function toRoomReactionItem(
  row: StoredEventRow,
  options: {
    roomId: string;
    occupantIds: Set<number>;
    knownRoomIds: Set<string>;
    chaosEventsAsc: ChaosEventItem[];
    stimulusEventsAsc: RoomStimulusHistoryItem[];
    allowAnyCandidateTarget?: boolean;
  },
): RoomReactionItem | null {
  const matchingChaos = [...options.chaosEventsAsc]
    .reverse()
    .find((chaos) => {
      const delta = row.tick - chaos.tick;
      return delta >= 0 && delta <= ROOM_REACTION_WINDOW_TICKS;
    }) ?? null;
  const matchingStimulus = [...options.stimulusEventsAsc]
    .reverse()
    .find((stimulus) => {
      const delta = row.tick - stimulus.tick;
      return delta >= 0 && delta <= ROOM_REACTION_WINDOW_TICKS;
    }) ?? null;
  try {
    const payload = JSON.parse(row.payload) as {
      agent_id?: unknown;
      action_type?: string;
      content?: string | null;
      target_room?: string | null;
    };
    const { agentId, numericId } = normalizeAgentId(
      payload.agent_id,
      row.aggregate_id,
    );
    const targetRoom = payload.target_room ? String(payload.target_room) : null;
    const candidateMatch =
      numericId != null && options.occupantIds.has(numericId);
    const belongsToRoom =
      targetRoom === options.roomId ||
      (options.allowAnyCandidateTarget
        ? candidateMatch
        : candidateMatch && (!targetRoom || !options.knownRoomIds.has(targetRoom)));
    if (!belongsToRoom) return null;
    const agentNameMap = getAgentNameMap();
    return {
      event_id: row.event_id,
      agent_id: agentId,
      agent_name:
        (numericId != null && agentNameMap.get(numericId)) || agentId,
      action_type: String(payload.action_type ?? ""),
      content: payload.content ? String(payload.content) : null,
      target_room: targetRoom,
      tick: row.tick,
      timestamp_ms: row.timestamp_ms,
      correlation_id: row.correlation_id,
      chaos_event_id: matchingChaos?.event_id ?? null,
      chaos_type: matchingChaos?.chaos_type ?? null,
      chaos_description: matchingChaos?.description ?? null,
      chaos_tick: matchingChaos?.tick ?? null,
      stimulus_event_id: matchingStimulus?.event_id ?? null,
      stimulus_type: matchingStimulus?.stimulus_type ?? null,
      stimulus_description: matchingStimulus?.description ?? null,
      stimulus_tick: matchingStimulus?.tick ?? null,
    };
  } catch {
    const { agentId, numericId } = normalizeAgentId(undefined, row.aggregate_id);
    if (numericId == null || !options.occupantIds.has(numericId)) return null;
    return {
      event_id: row.event_id,
      agent_id: agentId,
      agent_name: row.aggregate_id,
      action_type: "",
      content: null,
      target_room: options.roomId,
      tick: row.tick,
      timestamp_ms: row.timestamp_ms,
      correlation_id: row.correlation_id,
      chaos_event_id: matchingChaos?.event_id ?? null,
      chaos_type: matchingChaos?.chaos_type ?? null,
      chaos_description: matchingChaos?.description ?? null,
      chaos_tick: matchingChaos?.tick ?? null,
      stimulus_event_id: matchingStimulus?.event_id ?? null,
      stimulus_type: matchingStimulus?.stimulus_type ?? null,
      stimulus_description: matchingStimulus?.description ?? null,
      stimulus_tick: matchingStimulus?.tick ?? null,
    };
  }
}

function toTransitReactionItem(
  row: StoredEventRow,
  options: {
    roomId: string;
    latestChaos: ChaosEventItem | null;
    latestStimulus: RoomStimulusHistoryItem | null;
  },
): RoomReactionItem | null {
  try {
    const payload = JSON.parse(row.payload) as {
      from_room?: string | null;
      to_room?: string | null;
    };
    if (payload.from_room !== options.roomId) return null;
    const { agentId, numericId } = normalizeAgentId(undefined, row.aggregate_id);
    const agentNameMap = getAgentNameMap();
    const toRoom = payload.to_room ? String(payload.to_room) : null;
    const content = toRoom ? `wechselt nach ${toRoom}` : "verlaesst den Raum";
    return {
      event_id: row.event_id,
      agent_id: agentId,
      agent_name:
        (numericId != null && agentNameMap.get(numericId)) || agentId,
      action_type: "Transit",
      content,
      target_room: toRoom,
      tick: row.tick,
      timestamp_ms: row.timestamp_ms,
      correlation_id: row.correlation_id,
      chaos_event_id: options.latestChaos?.event_id ?? null,
      chaos_type: options.latestChaos?.chaos_type ?? null,
      chaos_description: options.latestChaos?.description ?? null,
      chaos_tick: options.latestChaos?.tick ?? null,
      stimulus_event_id: options.latestStimulus?.event_id ?? null,
      stimulus_type: options.latestStimulus?.stimulus_type ?? null,
      stimulus_description: options.latestStimulus?.description ?? null,
      stimulus_tick: options.latestStimulus?.tick ?? null,
    };
  } catch {
    return null;
  }
}

function finalizeRoomReactions(
  items: RoomReactionItem[],
  limit: number,
): RoomReactionItem[] {
  const deduped = new Map<string, RoomReactionItem>();
  for (const item of items) {
    deduped.set(item.event_id, item);
  }
  return [...deduped.values()]
    .sort((a, b) => {
      const aContextScore = (a.stimulus_event_id ? 2 : 0) + (a.chaos_event_id ? 1 : 0);
      const bContextScore = (b.stimulus_event_id ? 2 : 0) + (b.chaos_event_id ? 1 : 0);
      if (aContextScore !== bContextScore) {
        return bContextScore - aContextScore;
      }
      if (a.timestamp_ms !== b.timestamp_ms) {
        return b.timestamp_ms - a.timestamp_ms;
      }
      return b.tick - a.tick;
    })
    .slice(0, limit)
    .reverse();
}

export function getRoomStimulusEventsByRoom(
  roomId: string,
  limit = 50,
): RoomStimulusHistoryItem[] {
  return eventStoreDb
    .query<
      {
        event_id: string;
        aggregate_id: string;
        payload: string;
        tick: number;
        timestamp_ms: number;
      },
      [string, number]
    >(
      `SELECT event_id, aggregate_id, payload, tick, timestamp_ms
       FROM events
       WHERE event_type = 'room_stimulus_applied'
         AND aggregate_id = ?
       ORDER BY id DESC
       LIMIT ?`,
    )
    .all(roomId, limit)
    .map((row) => {
      let stimulusType = "unknown";
      let delta = 0;
      let description = "";
      try {
        const payload = JSON.parse(row.payload) as {
          room_id?: string;
          stimulus_type?: string;
          delta?: number;
          description?: string;
        };
        stimulusType = String(payload.stimulus_type ?? "unknown");
        delta = typeof payload.delta === "number" ? payload.delta : 0;
        description = String(payload.description ?? "");
      } catch {
        // ignore parse errors
      }
      return {
        event_id: row.event_id,
        room_id: row.aggregate_id,
        stimulus_type: stimulusType,
        delta,
        description,
        tick: row.tick,
        timestamp_ms: row.timestamp_ms,
      };
    })
    .reverse();
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

// ── Agent Name Lookup (cached) ───────────────────

let _agentNameCache: Map<number, string> | null = null;
let _agentNameCacheTime = 0;

export function getAgentNameMap(): Map<number, string> {
  const now = Date.now();
  // Refresh cache every 5s (war 60s — #253 stale cache fix)
  if (_agentNameCache && now - _agentNameCacheTime < 5_000) {
    return _agentNameCache;
  }
  const rows = projectionDb
    .query<{ agent_id: number; name: string }, []>(
      "SELECT agent_id, name FROM agent_live_view",
    )
    .all();
  _agentNameCache = new Map(rows.map((r) => [r.agent_id, r.name]));
  _agentNameCacheTime = now;
  return _agentNameCache;
}

// ── Change Detection (fuer WebSocket) ────────────

export function getGlobalMaxEventId(): number {
  const row = projectionDb
    .query<{ max_id: number | null }, []>(
      `SELECT MAX(m) as max_id
       FROM (
         SELECT MAX(last_event_id) as m FROM agent_live_view
         UNION ALL
         SELECT MAX(last_event_id) as m FROM room_live_view
       )`,
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
  "platform_analysis",
  "platform_intervention",
] as const;

const INCIDENT_TYPES_SQL = INCIDENT_EVENT_TYPES.map((t) => `'${t}'`).join(",");

export function getRecentIncidentEvents(hours: number, limit = 200): EventRow[] {
  const cutoff = Date.now() - hours * 3600_000;
  return eventStoreDb
    .query<EventRow, [number, number]>(
      `SELECT ${eventRowSelectColumns()}
       FROM events
       WHERE event_type IN (${INCIDENT_TYPES_SQL})
         AND timestamp_ms > ?
       ORDER BY id DESC
       LIMIT ?`,
    )
    .all(cutoff, limit);
}

export function getRecentPlatformAnalyses(limit = 50): PlatformAnalysisItem[] {
  const rows = eventStoreDb
    .query<
      {
        event_id: string;
        aggregate_id: string;
        payload: string;
        tick: number;
        timestamp_ms: number;
      },
      [number]
    >(
      `SELECT event_id, aggregate_id, payload, tick, timestamp_ms
       FROM events
       WHERE event_type = 'platform_analysis'
       ORDER BY id DESC
       LIMIT ?`,
    )
    .all(limit);

  return rows
    .map((row) => {
      try {
        const payload = JSON.parse(row.payload) as Record<string, unknown>;
        const unresolvedKeys = Array.isArray(payload.unresolved_keys)
          ? payload.unresolved_keys
              .map((value) => String(value).trim())
              .filter((value) => value.length > 0)
          : [];
        const parameters =
          payload.parameters && typeof payload.parameters === "object"
            ? (payload.parameters as Record<string, unknown>)
            : {};

        return {
          event_id: row.event_id,
          aggregate_id: row.aggregate_id,
          trigger: String(payload.trigger ?? "unknown"),
          severity: String(payload.severity ?? "info"),
          summary: String(payload.summary ?? ""),
          recommendation: String(payload.recommendation ?? ""),
          suggested_action:
            payload.suggested_action == null
              ? null
              : String(payload.suggested_action),
          target: String(payload.target ?? row.aggregate_id),
          provider: payload.provider == null ? null : String(payload.provider),
          model: payload.model == null ? null : String(payload.model),
          unresolved_keys: unresolvedKeys,
          parameters,
          tick: row.tick,
          timestamp_ms: row.timestamp_ms,
        } satisfies PlatformAnalysisItem;
      } catch {
        return null;
      }
    })
    .filter((row): row is PlatformAnalysisItem => row !== null);
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
      `SELECT ${eventRowSelectColumns()}
       FROM events
       WHERE correlation_id = ?
       ORDER BY id ASC`,
    )
    .all(correlationId);
}

export function getEventsByCausation(eventId: string): EventRow[] {
  return eventStoreDb
    .query<EventRow, [string]>(
      `SELECT ${eventRowSelectColumns()}
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
      `SELECT ${eventRowSelectColumns()}
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
      `SELECT ${eventRowSelectColumns()}
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

// ── Operator Messages (Chat Input) ──────────────

// Operator chat uses its own SQLite DB (EventStore is read-only for dashboard)
let _chatDb: InstanceType<typeof Database> | null = null;
function getChatDb(): InstanceType<typeof Database> {
  if (!_chatDb) {
    const chatDbPath = process.env.CHAT_DB || "./operator-chat.db";
    _chatDb = new Database(chatDbPath, { create: true });
    _chatDb.run(`CREATE TABLE IF NOT EXISTS operator_messages (
      id INTEGER PRIMARY KEY AUTOINCREMENT,
      message TEXT NOT NULL,
      room TEXT,
      gateway_status TEXT DEFAULT 'pending',
      gateway_response TEXT,
      created_at INTEGER NOT NULL
    )`);
    // Migrate: add gateway_response column if missing (existing DBs)
    try {
      _chatDb.run("ALTER TABLE operator_messages ADD COLUMN gateway_response TEXT");
    } catch { /* column already exists */ }
  }
  return _chatDb;
}

export function insertOperatorMessage(message: string, room: string | null): number {
  const db = getChatDb();
  db.query("INSERT INTO operator_messages (message, room, created_at) VALUES (?, ?, ?)")
    .run(message, room, Date.now());
  const row = db
    .query<{ id: number }, []>("SELECT last_insert_rowid() as id")
    .get();
  return row?.id ?? 0;
}

export function updateOperatorMessageGateway(
  id: number,
  status: string,
  responseContent: string | null,
): void {
  const db = getChatDb();
  db.query("UPDATE operator_messages SET gateway_status = ?, gateway_response = ? WHERE id = ?")
    .run(status, responseContent, id);
}

interface OperatorMessageRow {
  id: number;
  message: string;
  room: string | null;
  gateway_status: string;
  gateway_response: string | null;
  created_at: number;
}

/** Returns operator messages (both outgoing + gateway responses) as ChatMessages for merging. */
export function getOperatorChatMessages(limit = 100): ChatMessage[] {
  const db = getChatDb();
  const rows = db
    .query<OperatorMessageRow, [number]>(
      `SELECT id, message, room, gateway_status, gateway_response, created_at
       FROM operator_messages
       ORDER BY id DESC
       LIMIT ?`,
    )
    .all(limit);

  const result: ChatMessage[] = [];
  for (const row of rows) {
    // Operator's outgoing message
    result.push({
      id: -(row.id * 2),  // negative IDs to avoid collision with EventStore IDs
      event_id: `operator-msg-${row.id}`,
      agent_id: "operator",
      agent_name: "Operator",
      action_type: "operator_message",
      content: row.message,
      target_room: row.room,
      tick: 0,
      timestamp_ms: row.created_at,
    });

    // Gateway (agent) response, if available
    if (row.gateway_response && row.gateway_status === "ok") {
      result.push({
        id: -(row.id * 2 + 1),
        event_id: `operator-resp-${row.id}`,
        agent_id: "gateway",
        agent_name: "Agent (Gateway)",
        action_type: "gateway_response",
        content: row.gateway_response,
        target_room: row.room,
        tick: 0,
        timestamp_ms: row.created_at + 1,  // +1ms to sort after the operator message
      });
    }
  }
  return result;
}

/** Returns operator messages filtered by room. */
export function getOperatorChatMessagesByRoom(roomId: string, limit = 50): ChatMessage[] {
  const db = getChatDb();
  const rows = db
    .query<OperatorMessageRow, [string, number]>(
      `SELECT id, message, room, gateway_status, gateway_response, created_at
       FROM operator_messages
       WHERE room = ?
       ORDER BY id DESC
       LIMIT ?`,
    )
    .all(roomId, limit);

  const result: ChatMessage[] = [];
  for (const row of rows) {
    result.push({
      id: -(row.id * 2),
      event_id: `operator-msg-${row.id}`,
      agent_id: "operator",
      agent_name: "Operator",
      action_type: "operator_message",
      content: row.message,
      target_room: row.room,
      tick: 0,
      timestamp_ms: row.created_at,
    });
    if (row.gateway_response && row.gateway_status === "ok") {
      result.push({
        id: -(row.id * 2 + 1),
        event_id: `operator-resp-${row.id}`,
        agent_id: "gateway",
        agent_name: "Agent (Gateway)",
        action_type: "gateway_response",
        content: row.gateway_response,
        target_room: row.room,
        tick: 0,
        timestamp_ms: row.created_at + 1,
      });
    }
  }
  return result;
}

// ── Chat Messages (Agent Actions) ───────────────

export function getRecentChatMessages(limit = 100): ChatMessage[] {
  const agentMessages = eventStoreDb
    .query<{ id: number; event_id: string; aggregate_id: string; payload: string; tick: number; timestamp_ms: number }, [number]>(
      `SELECT id, event_id, aggregate_id, payload, tick, timestamp_ms
       FROM events
       WHERE event_type = 'agent_action_received'
       ORDER BY id DESC
       LIMIT ?`,
    )
    .all(limit)
    .map(toChatMessage);

  const operatorMessages = getOperatorChatMessages(limit);

  // Merge and sort by timestamp descending, take top N
  return [...agentMessages, ...operatorMessages]
    .sort((a, b) => b.timestamp_ms - a.timestamp_ms)
    .slice(0, limit);
}

export function getChatMessagesByRoom(roomId: string, limit = 50): ChatMessage[] {
  const agentMessages = eventStoreDb
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

  const operatorMessages = getOperatorChatMessagesByRoom(roomId, limit);

  return [...agentMessages, ...operatorMessages]
    .sort((a, b) => b.timestamp_ms - a.timestamp_ms)
    .slice(0, limit);
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
    const nameMap = getAgentNameMap();
    const numId = parseInt(agentId, 10);
    agentName = (!isNaN(numId) && nameMap.get(numId)) || p.name || agentId;
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
  const result: Record<string, string[]> = {};
  for (const [roomId, data] of Object.entries(getProjectedOccupantsByRoom())) {
    result[roomId] = data.names.slice();
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
      `SELECT ${eventRowSelectColumns()}
       FROM events
       WHERE event_type IN (${ACTIVITY_TYPES_SQL})
       ORDER BY id DESC
       LIMIT ?`,
    )
    .all(limit);
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
