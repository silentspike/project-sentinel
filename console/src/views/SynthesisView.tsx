import { createSignal, For, Show, onMount, onCleanup, type JSX } from "solid-js";
import {
  apiJson,
  postJson,
  patchJson,
  type SynthesisRule,
  type TrafficResponse,
  type JudgeAlert,
} from "../api";
import { VirtualScroller } from "../components/VirtualScroller";

// #429: normalize the gateway's agent_id (numeric "8") to the canonical "AGENT-08" used as the
// events.db aggregate_id, so the Judge column can join. Guard (R3-Minor 1): already-prefixed stays
// as-is; empty / non-numeric -> no join (never a phantom "AGENT-00").
export function canonicalAgentId(raw: string | undefined): string | null {
  if (!raw) return null;
  if (raw.startsWith("AGENT-")) return raw;
  if (/^\d+$/.test(raw)) return `AGENT-${raw.padStart(2, "0")}`;
  return null;
}

function timeLabel(iso: string): string {
  const d = new Date(iso);
  return Number.isNaN(d.getTime()) ? "—" : d.toLocaleTimeString();
}

export function SynthesisView(): JSX.Element {
  const [rules, setRules] = createSignal<SynthesisRule[]>([]);
  const [responses, setResponses] = createSignal<TrafficResponse[]>([]);
  const [alerts, setAlerts] = createSignal<JudgeAlert[]>([]);
  const [synthesisEnabled, setSynthesisEnabled] = createSignal(true);
  const [filter, setFilter] = createSignal("");
  const [feedback, setFeedback] = createSignal<{ text: string; kind: "ok" | "error" } | null>(null);
  const [busy, setBusy] = createSignal<string | null>(null);

  async function load(): Promise<void> {
    const [rulesR, statsR, respR, alertsR] = await Promise.allSettled([
      apiJson<SynthesisRule[]>("/api/control/synthesis-rules"),
      apiJson<{ synthesis_enabled?: boolean }>("/api/control/traffic-stats"),
      apiJson<TrafficResponse[]>("/api/control/traffic-responses"),
      apiJson<JudgeAlert[]>("/api/control/judge-alerts"),
    ]);
    if (rulesR.status === "fulfilled" && Array.isArray(rulesR.value)) setRules(rulesR.value);
    if (statsR.status === "fulfilled" && statsR.value)
      setSynthesisEnabled(Boolean(statsR.value.synthesis_enabled));
    if (respR.status === "fulfilled" && Array.isArray(respR.value)) setResponses(respR.value);
    if (alertsR.status === "fulfilled" && Array.isArray(alertsR.value)) setAlerts(alertsR.value);
  }

  onMount(() => {
    void load();
    const timer = window.setInterval(() => void load(), 10_000);
    onCleanup(() => window.clearInterval(timer));
  });

  async function toggleRule(name: string, enabled: boolean): Promise<void> {
    setBusy(name);
    setFeedback(null);
    try {
      await postJson(`/api/control/synthesis-rules/${name}`, { enabled });
      await load();
      setFeedback({ text: `Rule ${name} ${enabled ? "aktiviert" : "deaktiviert"}`, kind: "ok" });
    } catch (e) {
      setFeedback({
        text: e instanceof Error ? e.message : `Toggle ${name} fehlgeschlagen`,
        kind: "error",
      });
    } finally {
      setBusy(null);
    }
  }

  async function toggleGlobal(enabled: boolean): Promise<void> {
    setBusy("__global__");
    setFeedback(null);
    try {
      await patchJson("/api/control/config", { synthesis_enabled: enabled });
      await load();
    } catch (e) {
      setFeedback({
        text: e instanceof Error ? e.message : "Synthesis-Toggle fehlgeschlagen",
        kind: "error",
      });
    } finally {
      setBusy(null);
    }
  }

  // Judge anomalies are agent-level: join by the canonical agent id (events.db aggregate_id).
  const alertByAgent = () => {
    const m = new Map<string, JudgeAlert>();
    for (const a of alerts()) m.set(a.agent_id, a);
    return m;
  };

  function judgeFor(r: TrafficResponse): JudgeAlert | null {
    const id = canonicalAgentId(r.agent_id);
    if (!id) return null;
    return alertByAgent().get(id) ?? null;
  }

  const filteredResponses = () => {
    const f = filter().trim().toLowerCase();
    if (!f) return responses();
    return responses().filter(
      (r) =>
        (r.agent_name ?? "").toLowerCase().includes(f) ||
        (r.agent_id ?? "").toLowerCase().includes(f) ||
        (r.decision ?? "").toLowerCase().includes(f),
    );
  };

  return (
    <div
      data-testid="view-synthesis"
      class="col"
      style={{ gap: "12px", padding: "12px", overflow: "auto", height: "100%" }}
    >
      <section class="control-card">
        <h3 style={{ margin: "0 0 8px" }}>Synthesis Rules</h3>
        <label style={{ display: "flex", "align-items": "center", gap: "8px", "margin-bottom": "8px" }}>
          <input
            type="checkbox"
            data-testid="synthesis-global-toggle"
            checked={synthesisEnabled()}
            disabled={busy() === "__global__"}
            onChange={(e) => void toggleGlobal(e.currentTarget.checked)}
          />
          <span>Synthesis global aktiv</span>
        </label>
        <div data-testid="synthesis-rules" class="col" style={{ gap: "4px" }}>
          <For each={rules()}>
            {(rule) => (
              <label style={{ display: "flex", "align-items": "center", gap: "8px" }}>
                <input
                  type="checkbox"
                  data-testid={`synthesis-rule-${rule.name}`}
                  checked={rule.enabled}
                  disabled={busy() === rule.name}
                  onChange={(e) => void toggleRule(rule.name, e.currentTarget.checked)}
                />
                <span style={{ "font-family": "monospace" }}>{rule.name}</span>
              </label>
            )}
          </For>
        </div>
        <Show when={feedback()}>
          {(f) => (
            <p
              data-testid="synthesis-feedback"
              style={{
                color: f().kind === "ok" ? "var(--ok, #6c6)" : "var(--danger)",
                "font-size": "13px",
              }}
            >
              {f().text}
            </p>
          )}
        </Show>
      </section>

      <section class="control-card" style={{ flex: "1", "min-height": "0" }}>
        <h3 style={{ margin: "0 0 4px" }}>Request Inspector</h3>
        <p class="muted" style={{ "margin-top": 0, "font-size": "12px" }}>
          Letzte {responses().length} Requests. Judge = letzte Anomalie (drift/quality/fatigue/swap),
          agent-level — leer = keine Anomalie (gesund).
        </p>
        <input
          data-testid="inspector-filter"
          placeholder="Filter: Agent / Decision"
          value={filter()}
          onInput={(e) => setFilter(e.currentTarget.value)}
          style={{ width: "100%", "margin-bottom": "8px" }}
        />
        <div data-testid="inspector-table">
          <VirtualScroller
            items={filteredResponses()}
            rowHeight={44}
            height={420}
            renderRow={(r) => {
              const judge = judgeFor(r);
              return (
                <div
                  data-testid="inspector-row"
                  style={{
                    display: "grid",
                    "grid-template-columns": "84px 120px 96px 150px 1fr 96px 140px",
                    gap: "8px",
                    width: "100%",
                    "font-size": "12px",
                  }}
                >
                  <span class="muted">{timeLabel(r.logged_at)}</span>
                  <span style={{ "font-family": "monospace" }}>{r.agent_name || r.agent_id || "—"}</span>
                  <span class="muted">{r.request_class || "—"}</span>
                  <span class="muted">
                    {r.provider}
                    {r.model ? ` / ${r.model}` : ""}
                  </span>
                  <span data-testid="inspector-decision" style={{ "font-family": "monospace" }}>
                    {r.decision || "—"}
                    {r.rule ? `: ${r.rule}` : ""}
                  </span>
                  <span data-testid="inspector-fourthwall">{r.fourth_wall ? `4W:${r.fourth_wall}` : ""}</span>
                  <span data-testid="inspector-judge">
                    {judge ? `${judge.alert_type}/${judge.severity}` : ""}
                  </span>
                </div>
              );
            }}
          />
        </div>
      </section>
    </div>
  );
}
