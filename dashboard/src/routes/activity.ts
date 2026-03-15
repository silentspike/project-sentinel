import { Hono } from "hono";
import { getRecentActivityEvents, getAgentNameMap } from "../db";
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

    // Agent-ID aus Payload extrahieren + Name-Lookup (#238)
    const nameMap = getAgentNameMap();
    if (p.agent_id) {
      const aid = typeof p.agent_id === "object" ? p.agent_id[0] : p.agent_id;
      agentId = String(aid);
    }
    // Resolve numeric ID to agent name
    const numId = parseInt(agentId, 10);
    const agentName = (!isNaN(numId) && nameMap.get(numId)) || p.name || agentId;

    switch (row.event_type) {
      case "agent_spawned":
        summary = `${p.name || agentName} gespawnt`;
        detail = p.role ? `Rolle: ${p.role}` : null;
        break;
      case "agent_despawned":
        summary = `${agentName} despawnt`;
        detail = p.reason ? `Grund: ${p.reason}` : null;
        break;
      case "agent_action_received":
        summary = p.content
          ? `${agentName}: ${String(p.content)}`
          : `${agentName}: ${p.action_type || "Aktion"}`;
        detail = p.action_type || null;
        room = p.target_room || null;
        break;
      case "transit_started":
        summary = `${agentName} unterwegs`;
        detail = `${p.from_room || "?"} → ${p.to_room || "?"}`;
        room = p.to_room || null;
        break;
      case "transit_completed":
        summary = `${agentName} angekommen`;
        room = p.room_id || null;
        detail = room ? `in ${room}` : null;
        break;
      case "chaos_triggered":
        summary = `Chaos: ${p.event_type || "?"}`;
        detail = p.description || null;
        room = p.target_room || row.aggregate_id;
        break;
      case "bio_action_performed":
        summary = `${agentName}: ${p.action || "Bio-Aktion"}`;
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
        summary = `${agentName} konsolidiert`;
        detail = p.episodes
          ? `${Array.isArray(p.episodes) ? p.episodes.length : 0} Episoden`
          : null;
        break;
      case "agent_consolidation_failed":
        summary = `${agentName} Konsolidierung fehlgeschlagen`;
        detail = p.error || null;
        break;
      case "agent_status_changed":
        summary = `${agentName} Status: ${p.new_status || "?"}`;
        break;
      case "bio_state_updated":
        summary = `${agentName} Bio-Update`;
        detail = `H:${(p.hunger * 100).toFixed(0)}% E:${(p.energy * 100).toFixed(0)}% S:${(p.stress * 100).toFixed(0)}%`;
        room = p.room_id || null;
        break;
      case "room_physics_updated":
        summary = `Raum ${p.room_id || agentId} Physik`;
        detail = `${p.temperature?.toFixed(1) || "?"}°C CO2:${p.co2_ppm?.toFixed(0) || "?"}ppm`;
        room = p.room_id || null;
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
