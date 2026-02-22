import { Hono } from "hono";
import { getActiveAgents, getAgentById, getAllAgents } from "../db";
import { ROOM_METADATA } from "../rooms-meta";
import type { AgentRow, AgentListItem, AgentDetail } from "../types";

export const agentRoutes = new Hono();

function toListItem(row: AgentRow): AgentListItem {
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
  };
}

function toDetail(row: AgentRow): AgentDetail {
  return {
    ...toListItem(row),
    shift_set: row.shift_set,
    last_action: row.last_action,
    last_action_tick: row.last_action_tick,
    last_event_id: row.last_event_id,
  };
}

agentRoutes.get("/agents", (c) => {
  const agents = getActiveAgents();
  return c.json(agents.map(toListItem));
});

agentRoutes.get("/agents/:id/state", (c) => {
  const idParam = c.req.param("id");
  const id = parseInt(idParam, 10);

  if (isNaN(id)) {
    // Name-basierter Lookup (slug -> Name)
    const slug = idParam.toLowerCase();
    const all = getAllAgents();
    const agent = all.find(
      (a) => a.name.toLowerCase().replace(/\s+/g, "-") === slug,
    );
    if (!agent) return c.json({ error: "Agent not found" }, 404);
    return c.json(toDetail(agent));
  }

  const agent = getAgentById(id);
  if (!agent) return c.json({ error: "Agent not found" }, 404);
  return c.json(toDetail(agent));
});
