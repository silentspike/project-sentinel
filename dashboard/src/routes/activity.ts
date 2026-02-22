import { Hono } from "hono";
import { getRecentActivityEvents } from "../db";
import type { EventRow } from "../types";

export const activityRoutes = new Hono();

interface ActivityItem {
  id: number;
  event_type: string;
  agent_id: string;
  summary: string;
  detail: string | null;
  room: string | null;
  tick: number;
  timestamp_ms: number;
}

function toActivityItem(row: EventRow): ActivityItem {
  let agentId = row.aggregate_id;
  let summary = row.event_type;
  let detail: string | null = null;
  let room: string | null = null;

  try {
    const p = JSON.parse(row.payload);

    // Agent-ID aus Payload extrahieren (falls vorhanden)
    if (p.agent_id) {
      const aid = typeof p.agent_id === "object" ? p.agent_id[0] : p.agent_id;
      agentId = String(aid);
    }

    switch (row.event_type) {
      case "agent_spawned":
        summary = `${p.name || agentId} gespawnt`;
        detail = p.role ? `Rolle: ${p.role}` : null;
        break;
      case "agent_despawned":
        summary = `${agentId} despawnt`;
        detail = p.reason ? `Grund: ${p.reason}` : null;
        break;
      case "agent_action_received":
        summary = p.content
          ? `${agentId}: ${String(p.content)}`
          : `${agentId}: ${p.action_type || "Aktion"}`;
        detail = p.action_type || null;
        room = p.target_room || null;
        break;
      case "transit_started":
        summary = `${agentId} unterwegs`;
        detail = `${p.from_room || "?"} → ${p.to_room || "?"}`;
        room = p.to_room || null;
        break;
      case "transit_completed":
        summary = `${agentId} angekommen`;
        room = p.room_id || null;
        detail = room ? `in ${room}` : null;
        break;
      case "chaos_triggered":
        summary = `Chaos: ${p.event_type || "?"}`;
        detail = p.description || null;
        room = p.target_room || row.aggregate_id;
        break;
      case "bio_action_performed":
        summary = `${agentId}: ${p.action || "Bio-Aktion"}`;
        detail = p.description || null;
        break;
      case "shift_transition_completed":
        summary = "Schichtwechsel";
        detail = p.removed_agents
          ? `${Array.isArray(p.removed_agents) ? p.removed_agents.length : 0} Agents entfernt`
          : null;
        break;
      case "nightrun_started":
        summary = "Nightrun gestartet";
        break;
      case "nightrun_completed":
        summary = "Nightrun abgeschlossen";
        detail = `${p.agents_consolidated ?? 0} konsolidiert, ${p.agents_failed ?? 0} fehlgeschlagen`;
        break;
      case "agent_consolidated":
        summary = `${agentId} konsolidiert`;
        detail = p.episodes
          ? `${Array.isArray(p.episodes) ? p.episodes.length : 0} Episoden`
          : null;
        break;
      case "agent_consolidation_failed":
        summary = `${agentId} Konsolidierung fehlgeschlagen`;
        detail = p.error || null;
        break;
      case "agent_status_changed":
        summary = `${agentId} Status: ${p.new_status || "?"}`;
        break;
    }
  } catch {
    /* payload parse error — use defaults */
  }

  return {
    id: row.id,
    event_type: row.event_type,
    agent_id: agentId,
    summary,
    detail,
    room,
    tick: row.tick,
    timestamp_ms: row.timestamp_ms,
  };
}

activityRoutes.get("/activity", (c) => {
  const limit = Math.min(
    Math.max(parseInt(c.req.query("limit") || "200", 10), 1),
    500,
  );
  const events = getRecentActivityEvents(limit);
  return c.json(events.map(toActivityItem));
});
