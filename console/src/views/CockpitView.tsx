import { createEffect, createMemo, createSignal, For, onCleanup, onMount, Show, type JSX } from "solid-js";
import { apiJson, type CockpitIncident, type CockpitResponse, type SloViolation } from "../api";
import { consoleStore } from "../stores/console";
import { formatDateTime } from "./format";

const SEVERITY_LABELS: Record<string, string> = {
  critical: "CRIT",
  high: "HIGH",
  medium: "MED",
  low: "LOW",
};

const STATUS_LABELS: Record<string, string> = {
  active: "Aktiv",
  pending: "Ausstehend",
  resolved: "Gelöst",
  failed: "Fehlgeschlagen",
};

const SLO_ITEMS = [
  ["Lag", "Projection Lag"],
  ["Nightrun", "Nightrun Failure-Rate"],
  ["Chaos", "Chaos-Frequenz"],
  ["Despawn", "Despawn-Rate"],
] as const;

function severityRank(severity: string): number {
  if (severity === "critical") return 0;
  if (severity === "high") return 1;
  if (severity === "medium") return 2;
  if (severity === "low") return 3;
  return 4;
}

function SloItem(props: { label: string; violation?: SloViolation }): JSX.Element {
  return (
    <div class={`slo-item ${props.violation ? "slo-item--violation" : "slo-item--ok"}`}>
      <span>{props.label}</span>
      <strong>{props.violation ? `${props.violation.current_value}/${props.violation.threshold}` : "OK"}</strong>
    </div>
  );
}

function IncidentItem(props: { incident: CockpitIncident }): JSX.Element {
  return (
    <article class="incident-item" data-testid="cockpit-incident" data-severity={props.incident.severity} data-status={props.incident.status}>
      <div class="incident-header">
        <span class={`severity-badge severity-badge--${props.incident.severity}`}>
          {SEVERITY_LABELS[props.incident.severity] ?? props.incident.severity}
        </span>
        <strong>{props.incident.summary}</strong>
        <span class={`incident-status incident-status--${props.incident.status}`}>
          {STATUS_LABELS[props.incident.status] ?? props.incident.status}
        </span>
      </div>
      <div class="incident-meta">
        Tick {props.incident.tick} | {formatDateTime(props.incident.timestamp_ms)}
        <Show when={props.incident.agent_id}> | {props.incident.agent_id}</Show>
        <Show when={props.incident.room_id}> | {props.incident.room_id}</Show>
      </div>
      <div class="incident-actions">
        <Show
          when={props.incident.actions.length > 0}
          fallback={<div class="detail-empty">Keine Massnahmen eingeleitet</div>}
        >
          <For each={props.incident.actions}>
            {(action) => <div class="incident-action">Aktion: {action.summary}</div>}
          </For>
        </Show>
      </div>
      <Show when={props.incident.outcome != null || props.incident.status === "pending"}>
        <div class={`incident-outcome ${props.incident.outcome == null ? "incident-outcome--pending" : ""}`}>
          Outcome: {props.incident.outcome ?? "ausstehend"}
        </div>
      </Show>
    </article>
  );
}

export function CockpitView(): JSX.Element {
  const [cockpit, setCockpit] = createSignal<CockpitResponse | null>(null);
  const [error, setError] = createSignal("");
  const [loading, setLoading] = createSignal(false);
  let lastFetch = 0;

  const loadCockpit = async () => {
    const now = Date.now();
    if (loading() || now - lastFetch < 1000) return;
    lastFetch = now;
    setLoading(true);
    try {
      setCockpit(await apiJson<CockpitResponse>("/api/cockpit"));
      setError("");
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  };

  onMount(() => {
    void loadCockpit();
    const timer = window.setInterval(() => void loadCockpit(), 5_000);
    onCleanup(() => window.clearInterval(timer));
  });

  createEffect(() => {
    const topic = consoleStore.lastTopic;
    if (topic === "kpi" || topic === "agent_live" || topic === "room_live") void loadCockpit();
  });

  const incidents = createMemo(() =>
    [...(cockpit()?.incidents ?? [])].sort((a, b) => {
      const rank = severityRank(a.severity) - severityRank(b.severity);
      if (rank !== 0) return rank;
      return b.timestamp_ms - a.timestamp_ms;
    }),
  );
  const activeIncidents = createMemo(() => incidents().filter((incident) => incident.status === "active" || incident.status === "pending"));
  const resolvedIncidents = createMemo(() => incidents().filter((incident) => incident.status === "resolved" || incident.status === "failed"));
  const violationByName = createMemo(() => new Map((cockpit()?.slo_violations ?? []).map((violation) => [violation.name, violation])));

  return (
    <section class="col view-panel" data-testid="view-cockpit">
      <div class="col__head view-head">
        <span>Cockpit</span>
        <span class={`pill ${cockpit()?.events_db === "ok" ? "pill-ok" : "pill-warn"}`}>
          events.db {cockpit()?.events_db ?? "lade"}
        </span>
      </div>
      <div class="col__body view-body">
        <Show when={!error()} fallback={<div class="detail-state detail-state--error">{error()}</div>}>
          <div class="slo-bar" data-testid="slo-bar">
            <For each={SLO_ITEMS}>
              {([label, key]) => <SloItem label={label} violation={violationByName().get(key)} />}
            </For>
          </div>

          <div class="cockpit-summary">
            {cockpit()?.total_active ?? 0} aktiv / {cockpit()?.total_resolved_24h ?? 0} abgeschlossen (24h)
          </div>

          <section class="incident-section">
            <h3>Aktive Incidents ({activeIncidents().length})</h3>
            <Show when={activeIncidents().length > 0} fallback={<div class="cockpit-empty">Keine aktiven Incidents</div>}>
              <div class="incident-list">
                <For each={activeIncidents()}>{(incident) => <IncidentItem incident={incident} />}</For>
              </div>
            </Show>
          </section>

          <Show when={resolvedIncidents().length > 0}>
            <section class="incident-section">
              <h3>Abgeschlossen (24h): {resolvedIncidents().length}</h3>
              <div class="incident-list incident-list--resolved">
                <For each={resolvedIncidents()}>{(incident) => <IncidentItem incident={incident} />}</For>
              </div>
            </section>
          </Show>
        </Show>
      </div>
    </section>
  );
}
