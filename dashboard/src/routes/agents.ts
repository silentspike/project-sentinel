import { Hono } from "hono";
import { getActiveAgents, getAgentById, getAllAgents } from "../db";
import { ROOM_METADATA } from "../rooms-meta";
import type { AgentRow, AgentListItem, AgentDetail } from "../types";

export const agentRoutes = new Hono();

// Cached stall data from Prometheus (refreshed every 10s)
let stalledAgentSet = new Set<string>();
let stallCacheTime = 0;
const STALL_CACHE_TTL_MS = 10_000;

async function refreshStallData(): Promise<void> {
  const now = Date.now();
  if (now - stallCacheTime < STALL_CACHE_TTL_MS) return;
  try {
    const resp = await fetch("http://localhost:9090/metrics", {
      signal: AbortSignal.timeout(2000),
    });
    if (!resp.ok) return;
    const text = await resp.text();
    const newSet = new Set<string>();
    const re = /sentinel_agent_stalled\{cgroup_id="[^"]*",agent="([^"]*)"\}\s+1/g;
    let m;
    while ((m = re.exec(text)) !== null) {
      newSet.add(m[1]);
    }
    stalledAgentSet = newSet;
    stallCacheTime = now;
  } catch {
    // Keep previous cache on error
  }
}

function toListItem(row: AgentRow, stalled: boolean): AgentListItem {
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
    stalled,
  };
}

function toDetail(row: AgentRow, stalled: boolean): AgentDetail {
  return {
    ...toListItem(row, stalled),
    shift_set: row.shift_set,
    last_action: row.last_action,
    last_action_tick: row.last_action_tick,
    last_event_id: row.last_event_id,
  };
}

function agentIdToName(agentId: number): string {
  return "AGENT-" + String(agentId).padStart(2, "0");
}

agentRoutes.get("/agents", async (c) => {
  await refreshStallData();
  const agents = getActiveAgents();
  return c.json(
    agents.map((a) =>
      toListItem(a, stalledAgentSet.has(agentIdToName(a.agent_id))),
    ),
  );
});

agentRoutes.get("/agents/:id/state", async (c) => {
  await refreshStallData();
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
    return c.json(
      toDetail(agent, stalledAgentSet.has(agentIdToName(agent.agent_id))),
    );
  }

  const agent = getAgentById(id);
  if (!agent) return c.json({ error: "Agent not found" }, 404);
  return c.json(toDetail(agent, stalledAgentSet.has(agentIdToName(id))));
});
