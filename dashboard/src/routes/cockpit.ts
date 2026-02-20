// Operator Cockpit: Incident-priorisierte View mit Actions/Outcomes/SLO.
// Entscheidbare Zustaende aus EventStore + personality_evolution.

import { Hono } from "hono";
import {
  getRecentIncidentEvents,
  getRecentEvolutionAlerts,
  getEventsByCorrelation,
  getEventsByCausation,
  getEventById,
  getChaosCountLastHour,
  getUnexpectedDespawnCount,
  getLastNightrunStats,
  getProjectionLag,
} from "../db";
import type {
  EventRow,
  EvolutionRow,
  CockpitIncident,
  CockpitAction,
  CockpitResponse,
  SloViolation,
  IncidentSeverity,
  IncidentStatus,
} from "../types";

export const cockpitRoutes = new Hono();

// ── SLO Thresholds ────────────────────────────────

const SLO_LAG_THRESHOLD = 100;
const SLO_NIGHTRUN_FAILURE_RATE = 0.1;
const SLO_CHAOS_PER_HOUR = 3;
const SLO_DESPAWN_PER_HOUR = 2;

// ── Severity Ordering ─────────────────────────────

const SEVERITY_ORDER: Record<IncidentSeverity, number> = {
  critical: 0,
  high: 1,
  medium: 2,
  low: 3,
};

// ── Event → Incident Mapping ──────────────────────

function eventSeverity(eventType: string, payload: unknown): IncidentSeverity {
  if (eventType === "chaos_triggered") return "high";
  if (eventType === "agent_consolidation_failed") return "high";
  if (eventType === "agent_despawned") return "medium";
  if (eventType === "nightrun_completed") {
    const p = payload as { agents_failed?: number };
    return (p.agents_failed ?? 0) > 0 ? "medium" : "low";
  }
  return "medium";
}

function evolutionSeverity(changeType: string): IncidentSeverity {
  if (changeType === "fatigue_spike") return "high";
  return "medium";
}

function parsePayload(raw: string): Record<string, unknown> {
  try {
    return JSON.parse(raw) as Record<string, unknown>;
  } catch {
    return {};
  }
}

function summarizeEvent(
  eventType: string,
  payload: Record<string, unknown>,
  aggregateId: string,
): string {
  switch (eventType) {
    case "chaos_triggered":
      return `Chaos: ${String(payload.type ?? "unknown")} in ${aggregateId}`;
    case "agent_consolidation_failed":
      return `Konsolidierung fehlgeschlagen: ${String(payload.agent_name ?? aggregateId)} — ${String(payload.error ?? "unknown")}`;
    case "agent_despawned":
      return `Agent despawned: ${aggregateId} (${String(payload.reason ?? "unknown")})`;
    case "nightrun_completed": {
      const failed = payload.agents_failed ?? 0;
      const consolidated = payload.agents_consolidated ?? 0;
      return `Nightrun: ${String(consolidated)} konsolidiert, ${String(failed)} fehlgeschlagen`;
    }
    default:
      return `${eventType} on ${aggregateId}`;
  }
}

function summarizeEvolution(row: EvolutionRow): string {
  const delta =
    row.old_value != null
      ? ` (${row.old_value} → ${row.new_value})`
      : ` (${row.new_value})`;
  switch (row.change_type) {
    case "drift":
      return `Drift: ${row.agent_id} ${row.field}${delta}`;
    case "fatigue_spike":
      return `Fatigue: ${row.agent_id} ${row.field}${delta}`;
    case "quality_shift":
      return `Quality: ${row.agent_id} ${row.field}${delta}`;
    default:
      return `${row.change_type}: ${row.agent_id} ${row.field}${delta}`;
  }
}

// ── Action Resolution ─────────────────────────────

function resolveActions(event: EventRow): CockpitAction[] {
  // Prefer correlation chain (groups all events in a transaction/run)
  if (event.correlation_id) {
    const correlated = getEventsByCorrelation(event.correlation_id);
    const actions = correlated
      .filter((e) => e.event_id !== event.event_id)
      .map(toAction);
    if (actions.length > 0) return actions;
  }
  // Fallback: direct causation (single-level children)
  const caused = getEventsByCausation(event.event_id);
  return caused.map(toAction);
}

function toAction(e: EventRow): CockpitAction {
  const payload = parsePayload(e.payload);
  return {
    event_id: e.event_id,
    event_type: e.event_type,
    agent_id: e.aggregate_id,
    summary: summarizeEvent(e.event_type, payload, e.aggregate_id),
    tick: e.tick,
  };
}

// ── Outcome Detection ─────────────────────────────

function determineOutcome(
  actions: CockpitAction[],
  event: EventRow,
): { status: IncidentStatus; outcome: string | null } {
  if (actions.length === 0) {
    // No follow-up — check if compensation was applied
    if (event.compensation_type !== "none") {
      return { status: "resolved", outcome: `Kompensation: ${event.compensation_type}` };
    }
    return { status: "pending", outcome: null };
  }

  const last = actions[actions.length - 1];

  // Resolved if follow-up is a completion event
  const resolvedTypes = [
    "transit_completed",
    "agent_consolidated",
    "bio_action_performed",
    "agent_spawned",
  ];
  if (resolvedTypes.includes(last.event_type)) {
    return { status: "resolved", outcome: last.summary };
  }

  // Failed if last action is also a failure
  const failedTypes = ["agent_consolidation_failed", "agent_despawned"];
  if (failedTypes.includes(last.event_type)) {
    return { status: "failed", outcome: last.summary };
  }

  // Active if there are actions but no clear resolution
  return { status: "active", outcome: last.summary };
}

// ── Build Incidents ───────────────────────────────

function buildEventIncident(event: EventRow): CockpitIncident {
  const payload = parsePayload(event.payload);
  const severity = eventSeverity(event.event_type, payload);

  // Skip nightrun_completed with 0 failures
  if (
    event.event_type === "nightrun_completed" &&
    (payload.agents_failed ?? 0) === 0
  ) {
    // Return as resolved low-severity (will be filtered)
    return {
      id: event.event_id,
      source: "event",
      incident_type: event.event_type,
      severity: "low",
      status: "resolved",
      agent_id: null,
      room_id: null,
      summary: summarizeEvent(event.event_type, payload, event.aggregate_id),
      tick: event.tick,
      timestamp_ms: event.timestamp_ms,
      actions: [],
      outcome: "Nightrun erfolgreich",
    };
  }

  const actions = resolveActions(event);
  const { status, outcome } = determineOutcome(actions, event);

  return {
    id: event.event_id,
    source: "event",
    incident_type: event.event_type,
    severity,
    status,
    agent_id: event.event_type === "agent_despawned" ||
      event.event_type === "agent_consolidation_failed"
      ? event.aggregate_id
      : null,
    room_id: event.event_type === "chaos_triggered"
      ? (payload.target_room as string | null) ?? event.aggregate_id
      : null,
    summary: summarizeEvent(event.event_type, payload, event.aggregate_id),
    tick: event.tick,
    timestamp_ms: event.timestamp_ms,
    actions,
    outcome,
  };
}

function buildEvolutionIncident(row: EvolutionRow): CockpitIncident {
  return {
    id: String(row.id),
    source: "evolution",
    incident_type: row.change_type,
    severity: evolutionSeverity(row.change_type),
    status: "active",
    agent_id: row.agent_id,
    room_id: null,
    summary: summarizeEvolution(row),
    tick: row.tick,
    timestamp_ms: row.created_at_ms,
    actions: [],
    outcome: null,
  };
}

// ── SLO Violations ────────────────────────────────

function buildSloViolations(): SloViolation[] {
  const violations: SloViolation[] = [];

  // Projection lag
  try {
    const lag = getProjectionLag();
    if (lag > SLO_LAG_THRESHOLD) {
      violations.push({
        name: "Projection Lag",
        current_value: lag,
        threshold: SLO_LAG_THRESHOLD,
        severity: lag > SLO_LAG_THRESHOLD * 5 ? "critical" : "high",
        description: `Lag ${lag} Events (Grenze: ${SLO_LAG_THRESHOLD})`,
      });
    }
  } catch {
    // EventStore unavailable
  }

  // Chaos frequency
  const chaosCount = getChaosCountLastHour();
  if (chaosCount > SLO_CHAOS_PER_HOUR) {
    violations.push({
      name: "Chaos-Frequenz",
      current_value: chaosCount,
      threshold: SLO_CHAOS_PER_HOUR,
      severity: "high",
      description: `${chaosCount} Chaos-Events/h (Grenze: ${SLO_CHAOS_PER_HOUR})`,
    });
  }

  // Unexpected despawns
  const despawnCount = getUnexpectedDespawnCount();
  if (despawnCount > SLO_DESPAWN_PER_HOUR) {
    violations.push({
      name: "Despawn-Rate",
      current_value: despawnCount,
      threshold: SLO_DESPAWN_PER_HOUR,
      severity: "medium",
      description: `${despawnCount} unerwartete Despawns/h (Grenze: ${SLO_DESPAWN_PER_HOUR})`,
    });
  }

  // Nightrun failure rate
  const nightrunStats = getLastNightrunStats();
  if (nightrunStats) {
    const total = nightrunStats.consolidated + nightrunStats.failed;
    if (total > 0) {
      const rate = nightrunStats.failed / total;
      if (rate > SLO_NIGHTRUN_FAILURE_RATE) {
        violations.push({
          name: "Nightrun Failure-Rate",
          current_value: Math.round(rate * 100),
          threshold: Math.round(SLO_NIGHTRUN_FAILURE_RATE * 100),
          severity: rate > 0.5 ? "critical" : "high",
          description: `${nightrunStats.failed}/${total} fehlgeschlagen (${Math.round(rate * 100)}%, Grenze: ${Math.round(SLO_NIGHTRUN_FAILURE_RATE * 100)}%)`,
        });
      }
    }
  }

  return violations;
}

// ── Main Endpoint ─────────────────────────────────

function buildCockpitResponse(hours: number): CockpitResponse {
  // Collect incidents from events
  const eventIncidents = getRecentIncidentEvents(hours)
    .map(buildEventIncident)
    .filter((i) => i.severity !== "low");

  // Collect incidents from personality evolution
  const evolutionIncidents = getRecentEvolutionAlerts(hours)
    .map(buildEvolutionIncident);

  // Merge and sort by severity (high first), then by timestamp (newest first)
  const allIncidents = [...eventIncidents, ...evolutionIncidents].sort(
    (a, b) => {
      const sevDiff = SEVERITY_ORDER[a.severity] - SEVERITY_ORDER[b.severity];
      if (sevDiff !== 0) return sevDiff;
      return b.timestamp_ms - a.timestamp_ms;
    },
  );

  const sloViolations = buildSloViolations();
  const totalActive = allIncidents.filter(
    (i) => i.status === "active" || i.status === "pending",
  ).length;
  const totalResolved = allIncidents.filter(
    (i) => i.status === "resolved",
  ).length;

  return {
    incidents: allIncidents,
    slo_violations: sloViolations,
    total_active: totalActive,
    total_resolved_24h: totalResolved,
  };
}

cockpitRoutes.get("/cockpit", (c) => {
  const hours = Math.min(
    Math.max(parseInt(c.req.query("hours") || "24", 10), 1),
    168,
  );
  return c.json(buildCockpitResponse(hours));
});

cockpitRoutes.get("/cockpit/incident/:id", (c) => {
  const eventId = c.req.param("id");
  const event = getEventById(eventId);
  if (!event) {
    return c.json({ error: "Incident not found" }, 404);
  }

  const incident = buildEventIncident(event);
  return c.json(incident);
});

// Export for testing
export { buildCockpitResponse };
