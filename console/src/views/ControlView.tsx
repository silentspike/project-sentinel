import { createEffect, createMemo, createSignal, For, onCleanup, onMount, Show, type JSX } from "solid-js";
import {
  apiJson,
  deleteJson,
  patchJson,
  postJson,
  type ControlConfig,
  type ControlStatus,
  type PlatformAnalysis,
  type PlatformState,
  type TrafficStats,
} from "../api";
import { formatNumber } from "./format";

function fallbackStatus(): ControlStatus {
  return { connected: false, paused: false, config: null, health: null, gateway: "offline" };
}

function boolText(value: unknown): string {
  return value ? "Ja" : "Nein";
}

function money(value: unknown): string {
  return typeof value === "number" ? `$${value.toFixed(2)}` : "--";
}

function percent(value: unknown): string {
  return typeof value === "number" ? `${(value * 100).toFixed(1)}%` : "--";
}

function compact(value: unknown): string {
  if (typeof value === "number") return formatNumber(value);
  if (typeof value === "boolean") return boolText(value);
  if (value == null || value === "") return "--";
  return String(value);
}

function Section(props: { title: string; children: JSX.Element }): JSX.Element {
  return (
    <section class="control-card">
      <h3>{props.title}</h3>
      {props.children}
    </section>
  );
}

function KvRow(props: { label: string; value: unknown; mono?: boolean }): JSX.Element {
  return (
    <div class="control-kv">
      <span>{props.label}</span>
      <strong class={props.mono ? "mono" : ""}>{compact(props.value)}</strong>
    </div>
  );
}

function JsonBlock(props: { value: unknown; empty?: string }): JSX.Element {
  return (
    <pre class="json-block">
      {props.value == null ? props.empty ?? "Nicht verfuegbar" : JSON.stringify(props.value, null, 2)}
    </pre>
  );
}

export function ControlView(): JSX.Element {
  const [status, setStatus] = createSignal<ControlStatus>(fallbackStatus());
  const [traffic, setTraffic] = createSignal<TrafficStats | null>(null);
  const [analyses, setAnalyses] = createSignal<PlatformAnalysis[]>([]);
  const [platform, setPlatform] = createSignal<PlatformState | null>(null);
  const [loading, setLoading] = createSignal(true);
  const [feedback, setFeedback] = createSignal<{ text: string; kind: "ok" | "error" } | null>(null);
  const [busy, setBusy] = createSignal<string | null>(null);

  const [temperature, setTemperature] = createSignal("0.7");
  const [maxTokens, setMaxTokens] = createSignal("4096");
  const [rateLimit, setRateLimit] = createSignal("10");
  const [primaryProvider, setPrimaryProvider] = createSignal("anthropic-direct");
  const [agentId, setAgentId] = createSignal("");
  const [agentProvider, setAgentProvider] = createSignal("anthropic-direct");
  const [driftThreshold, setDriftThreshold] = createSignal("0.7");
  const [nudge, setNudge] = createSignal("");

  const config = createMemo(() => status().config);
  const gatewayOffline = createMemo(() => status().gateway === "offline" || !status().connected);

  const providerOptions = createMemo(() => {
    const values = new Set(["anthropic-direct", "claude-code"]);
    if (config()?.primary_provider) values.add(String(config()?.primary_provider));
    for (const provider of Object.values(config()?.agent_overrides ?? {})) values.add(provider);
    return [...values];
  });

  async function load(): Promise<void> {
    const [statusResult, trafficResult, analysesResult, platformResult] = await Promise.allSettled([
      apiJson<ControlStatus>("/api/control/status"),
      apiJson<TrafficStats>("/api/control/traffic-stats"),
      apiJson<PlatformAnalysis[] | { analyses?: PlatformAnalysis[] }>("/api/control/platform-analyses"),
      apiJson<PlatformState>("/api/control/platform-state"),
    ]);

    setStatus(statusResult.status === "fulfilled" ? statusResult.value : fallbackStatus());
    setTraffic(trafficResult.status === "fulfilled" ? trafficResult.value : null);
    if (analysesResult.status === "fulfilled") {
      const value = analysesResult.value;
      setAnalyses(Array.isArray(value) ? value : Array.isArray(value.analyses) ? value.analyses : []);
    } else {
      setAnalyses([]);
    }
    setPlatform(platformResult.status === "fulfilled" ? platformResult.value : null);
    setLoading(false);
  }

  async function runAction(label: string, fn: () => Promise<unknown>): Promise<void> {
    setBusy(label);
    setFeedback(null);
    try {
      await fn();
      setFeedback({ text: `${label} ausgefuehrt`, kind: "ok" });
      await load();
    } catch (error) {
      setFeedback({ text: error instanceof Error ? error.message : `${label} fehlgeschlagen`, kind: "error" });
    } finally {
      setBusy(null);
    }
  }

  async function patchConfig(updates: Partial<ControlConfig>, label = "Config"): Promise<void> {
    await runAction(label, () => patchJson("/api/control/config", updates));
  }

  createEffect(() => {
    const cfg = config();
    if (!cfg) return;
    setTemperature(String(cfg.temperature ?? 0.7));
    setMaxTokens(String(cfg.max_tokens ?? 4096));
    setRateLimit(String(cfg.rate_limit_rps ?? 10));
    setPrimaryProvider(String(cfg.primary_provider ?? "anthropic-direct"));
    setDriftThreshold(String(cfg.drift_threshold ?? 0.7));
    setNudge(String(cfg.narrative_nudge ?? ""));
  });

  onMount(() => {
    void load();
    const timer = window.setInterval(() => void load(), 10_000);
    onCleanup(() => window.clearInterval(timer));
  });

  return (
    <section class="col view-panel" data-testid="view-control">
      <div class="col__head view-head">
        <span>Control</span>
        <span class={`pill ${gatewayOffline() ? "pill-warn" : "pill-ok"}`} data-testid="control-gateway">
          {gatewayOffline() ? "Gateway offline" : status().paused ? "Pausiert" : "Aktiv"}
        </span>
      </div>
      <div class="col__body control-shell">
        <Show when={feedback()}>
          {(item) => <div class={`trigger-feedback trigger-feedback--${item().kind}`}>{item().text}</div>}
        </Show>
        <Show when={!loading()} fallback={<p class="muted">Lade Control-Status...</p>}>
          <div class="control-grid">
            <Section title="Quick Actions">
              <div class="control-action-row">
                <button
                  class={status().paused ? "primary" : ""}
                  disabled={busy() !== null}
                  data-testid="control-pause-resume"
                  onClick={() => void runAction(status().paused ? "Resume" : "Pause", () => postJson(status().paused ? "/api/control/resume" : "/api/control/pause", {}))}
                >
                  {status().paused ? "Resume" : "Pause"}
                </button>
                <button disabled={busy() !== null} onClick={() => void runAction("Nightrun", () => postJson("/api/control/nightrun", {}))}>
                  Nightrun
                </button>
                <button disabled={busy() !== null} onClick={() => void runAction("Platform-Analyse", () => postJson("/api/control/platform-analyze", {}))}>
                  Platform-Analyse
                </button>
                <button disabled={busy() !== null} onClick={() => void load()}>Refresh</button>
              </div>
              <div class="control-status-grid">
                <KvRow label="Gateway" value={status().gateway ?? (status().connected ? "ok" : "offline")} />
                <KvRow label="Paused" value={status().paused} />
                <KvRow label="Saved Rate" value={status().saved_rate_limit ?? "--"} />
              </div>
            </Section>

            <Section title="Provider">
              <Show when={config()} fallback={<div class="degraded-panel">Gateway offline</div>}>
                <div class="form-grid">
                  <label>
                    Primary Provider
                    <select value={primaryProvider()} onChange={(event) => {
                      const provider = event.currentTarget.value;
                      setPrimaryProvider(provider);
                      void runAction("Provider", () => postJson("/api/control/provider", { provider }));
                    }}>
                      <For each={providerOptions()}>{(provider) => <option value={provider}>{provider}</option>}</For>
                    </select>
                  </label>
                  <label>
                    Agent ID
                    <input value={agentId()} placeholder="agent-1" onInput={(event) => setAgentId(event.currentTarget.value)} />
                  </label>
                  <label>
                    Override Provider
                    <select value={agentProvider()} onChange={(event) => setAgentProvider(event.currentTarget.value)}>
                      <For each={providerOptions()}>{(provider) => <option value={provider}>{provider}</option>}</For>
                    </select>
                  </label>
                  <button
                    disabled={!agentId().trim() || busy() !== null}
                    onClick={() => void runAction("Agent-Provider", () => postJson("/api/control/agent-provider", { agent_id: agentId().trim(), provider: agentProvider() }))}
                  >
                    Setzen
                  </button>
                </div>
                <div class="override-list">
                  <For each={Object.entries(config()?.agent_overrides ?? {})}>
                    {([id, provider]) => (
                      <span class="pill">
                        {id}: {provider}
                        <button
                          class="inline-x"
                          title="Override entfernen"
                          onClick={() => void runAction("Override entfernen", () => deleteJson("/api/control/agent-provider", { agent_id: id }))}
                        >
                          x
                        </button>
                      </span>
                    )}
                  </For>
                </div>
              </Show>
            </Section>

            <Section title="LLM Parameter">
              <Show when={config()} fallback={<div class="degraded-panel">Gateway offline</div>}>
                <div class="form-grid form-grid--three">
                  <label>Temperature<input type="number" min="0" max="2" step="0.1" value={temperature()} onInput={(e) => setTemperature(e.currentTarget.value)} /></label>
                  <label>Max Tokens<input type="number" min="1" step="256" value={maxTokens()} onInput={(e) => setMaxTokens(e.currentTarget.value)} /></label>
                  <label>Rate Limit<input type="number" min="0" step="1" value={rateLimit()} onInput={(e) => setRateLimit(e.currentTarget.value)} /></label>
                  <button
                    class="primary"
                    disabled={busy() !== null}
                    onClick={() => void patchConfig({
                      temperature: Number(temperature()),
                      max_tokens: Number.parseInt(maxTokens(), 10),
                      rate_limit_rps: Number(rateLimit()),
                    }, "LLM Parameter")}
                  >
                    Anwenden
                  </button>
                </div>
              </Show>
            </Section>

            <Section title="Pipeline Hardening">
              <Show when={config()} fallback={<div class="degraded-panel">Gateway offline</div>}>
                <div class="control-stack">
                  <label class="toggle-row">
                    <input
                      type="checkbox"
                      checked={Boolean(config()?.personality_guard_enabled)}
                      onChange={(event) => void patchConfig({ personality_guard_enabled: event.currentTarget.checked }, "Personality Guard")}
                    />
                    Personality Guard
                  </label>
                  <label class="range-row">
                    Drift Threshold <strong>{Number(driftThreshold()).toFixed(2)}</strong>
                    <input
                      type="range"
                      min="0"
                      max="1"
                      step="0.05"
                      value={driftThreshold()}
                      onInput={(event) => setDriftThreshold(event.currentTarget.value)}
                      onChange={() => void patchConfig({ drift_threshold: Number(driftThreshold()) }, "Drift Threshold")}
                    />
                  </label>
                  <label class="toggle-row">
                    <input
                      type="checkbox"
                      checked={Boolean(config()?.quality_gate_enabled)}
                      onChange={(event) => void patchConfig({ quality_gate_enabled: event.currentTarget.checked }, "Quality Gate")}
                    />
                    Quality Gate
                  </label>
                  <div class="form-grid">
                    <label>Narrative Nudge<textarea rows={3} value={nudge()} onInput={(e) => setNudge(e.currentTarget.value)} /></label>
                    <button onClick={() => void patchConfig({ narrative_nudge: nudge() }, "Narrative Nudge")}>Nudge setzen</button>
                  </div>
                </div>
              </Show>
            </Section>

            <Section title="Guardrails">
              <Show when={status().health} fallback={<div class="degraded-panel">Health offline</div>}>
                <div class="control-status-grid">
                  <KvRow label="Guardrails" value={status().health?.guardrails_enabled} />
                  <KvRow label="Circuit Breakers" value={JSON.stringify(status().health?.circuit_breakers ?? "N/A")} mono />
                </div>
              </Show>
            </Section>

            <Section title="Traffic Stats">
              <Show when={traffic()} fallback={<div class="degraded-panel">Traffic stats offline</div>}>
                <div class="control-status-grid">
                  <KvRow label="Primary" value={traffic()?.primary_provider} />
                  <KvRow label="Internal" value={traffic()?.internal_primary_provider ?? traffic()?.primary_provider} />
                  <KvRow label="MITM" value={traffic()?.external_mitm_provider} />
                  <KvRow label="Kosten heute" value={money(traffic()?.current_cost_usd)} />
                  <KvRow label="Ersparnis heute" value={money(traffic()?.estimated_savings_usd)} />
                  <KvRow label="Forward Calls" value={traffic()?.forward_calls} />
                  <KvRow label="Synthesis Rate" value={percent(traffic()?.synthesis_rate)} />
                  <KvRow label="Queue Depth" value={traffic()?.queue_depth} />
                  <KvRow label="Intercept Mode" value={traffic()?.intercept_mode} />
                </div>
              </Show>
            </Section>

            <Section title="Platform Analyses">
              <Show when={analyses().length > 0} fallback={<div class="degraded-panel">Keine Platform-Analysen</div>}>
                <div class="analysis-list">
                  <For each={analyses().slice(0, 8)}>
                    {(analysis) => (
                      <article class="analysis-item" data-severity={analysis.severity ?? "info"}>
                        <div class="analysis-item__head">
                          <strong>{analysis.summary ?? "(ohne Summary)"}</strong>
                          <span class="pill">{String(analysis.severity ?? "info").toUpperCase()}</span>
                        </div>
                        <p>{analysis.recommendation ?? "Keine Empfehlung"}</p>
                        <div class="analysis-item__meta">
                          <span>{analysis.trigger ?? "--"}</span>
                          <span>{analysis.suggested_action ?? "--"}</span>
                          <span>T{analysis.tick ?? "--"}</span>
                        </div>
                      </article>
                    )}
                  </For>
                </div>
              </Show>
            </Section>

            <Section title="Platform State">
              <Show when={platform()} fallback={<div class="degraded-panel">Platform-State offline</div>}>
                <div class="control-status-grid">
                  <KvRow label="Current Tick" value={platform()?.current_tick} />
                  <KvRow label="LLM Enabled" value={platform()?.llm_enabled} />
                  <KvRow label="Last Analysis Tick" value={platform()?.last_analysis_tick} />
                  <KvRow label="Retry Delay" value={`${platform()?.llm_retry_delay_secs ?? "--"}s`} />
                </div>
                <div class="mini-table">
                  <div class="mini-table__head">Agent</div>
                  <div class="mini-table__head">Profile</div>
                  <div class="mini-table__head">Activity</div>
                  <For each={(platform()?.agents ?? []).slice(0, 10)}>
                    {(agent) => (
                      <>
                        <div>{agent.name ?? agent.agent_id ?? "--"}</div>
                        <div>{agent.current_profile ?? "--"}</div>
                        <div>{agent.last_activity_tick ?? "--"}</div>
                      </>
                    )}
                  </For>
                </div>
              </Show>
            </Section>

            <Section title="Live Config">
              <JsonBlock value={config()} />
            </Section>
          </div>
        </Show>
      </div>
    </section>
  );
}
