import { createMemo, createSignal, For, onCleanup, onMount, Show, type JSX } from "solid-js";
import {
  apiJson,
  apiText,
  postJson,
  type GaiaAlert,
  type GaiaAlertsResponse,
  type GaiaSessionIndexEntry,
  type GaiaSessionRun,
  type GaiaSessionsResponse,
} from "../api";
import { addToast } from "../components/controls";

type GaiaMode = "deep" | "setup";

function timeLabel(ms: number | null | undefined): string {
  if (!ms) return "--";
  const date = new Date(ms);
  return Number.isNaN(date.getTime()) ? "--" : date.toLocaleString();
}

function totalTokens(session: GaiaSessionIndexEntry): number {
  const usage = session.usage;
  return (
    usage.input_tokens +
    usage.output_tokens +
    usage.cache_read_input_tokens +
    usage.cache_creation_input_tokens
  );
}

function costLabel(session: GaiaSessionIndexEntry): string {
  return typeof session.usage.total_cost_usd === "number"
    ? `$${session.usage.total_cost_usd.toFixed(4)}`
    : "$0.0000";
}

function statusClass(status: string): string {
  if (status === "succeeded") return "pill pill-ok";
  if (status === "failed" || status === "timed_out") return "pill pill-warn";
  return "pill";
}

function kindLabel(kind: string): string {
  return kind === "setup_interview" ? "Setup" : "Deep";
}

function sortAlerts(alerts: GaiaAlert[]): GaiaAlert[] {
  return [...alerts].sort((a, b) => b.timestamp_ms - a.timestamp_ms);
}

function sortSessions(sessions: GaiaSessionIndexEntry[]): GaiaSessionIndexEntry[] {
  return [...sessions].sort((a, b) => b.started_at_ms - a.started_at_ms);
}

export function GaiaConsoleView(): JSX.Element {
  const [alerts, setAlerts] = createSignal<GaiaAlert[]>([]);
  const [sessions, setSessions] = createSignal<GaiaSessionIndexEntry[]>([]);
  const [stream, setStream] = createSignal("");
  const [selectedSessionId, setSelectedSessionId] = createSignal("");
  const [mode, setMode] = createSignal<GaiaMode>("deep");
  const [prompt, setPrompt] = createSignal("");
  const [resume, setResume] = createSignal("");
  const [loading, setLoading] = createSignal(true);
  const [busy, setBusy] = createSignal<"" | "load" | "session" | "stream">("");
  const [feedback, setFeedback] = createSignal<{ text: string; kind: "ok" | "error" } | null>(null);

  const latestAlerts = createMemo(() => sortAlerts(alerts()).slice(0, 12));
  const latestSessions = createMemo(() => sortSessions(sessions()).slice(0, 12));
  const selectedSession = createMemo(
    () => sessions().find((session) => session.gaia_session_id === selectedSessionId()) ?? null,
  );
  const canStart = createMemo(() => prompt().trim().length > 0 && busy() !== "session");

  async function load(): Promise<void> {
    setBusy((current) => current || "load");
    try {
      const [alertsResult, sessionsResult] = await Promise.allSettled([
        apiJson<GaiaAlertsResponse>("/api/gaia/alerts"),
        apiJson<GaiaSessionsResponse>("/api/gaia/sessions"),
      ]);
      if (alertsResult.status === "fulfilled") setAlerts(alertsResult.value.alerts ?? []);
      if (sessionsResult.status === "fulfilled") {
        const rows = sessionsResult.value.sessions ?? [];
        setSessions(rows);
        if (!selectedSessionId() && rows.length > 0) setSelectedSessionId(sortSessions(rows)[0].gaia_session_id);
      }
      if (alertsResult.status === "rejected" || sessionsResult.status === "rejected") {
        throw new Error("Gaia Console read failed");
      }
    } catch (error) {
      const text = error instanceof Error ? error.message : "Gaia Console load failed";
      setFeedback({ text, kind: "error" });
    } finally {
      setLoading(false);
      setBusy((current) => (current === "load" ? "" : current));
    }
  }

  async function loadStream(sessionId: string): Promise<void> {
    setSelectedSessionId(sessionId);
    setBusy("stream");
    setFeedback(null);
    try {
      const body = await apiText(`/api/gaia/sessions/${encodeURIComponent(sessionId)}/stream`);
      setStream(body);
    } catch (error) {
      const text = error instanceof Error ? error.message : "Stream laden fehlgeschlagen";
      setFeedback({ text, kind: "error" });
      setStream("");
    } finally {
      setBusy("");
    }
  }

  async function startSession(): Promise<void> {
    const text = prompt().trim();
    if (!text) return;
    const endpoint = mode() === "setup" ? "/api/gaia/setup-interview" : "/api/gaia/deep";
    setBusy("session");
    setFeedback(null);
    try {
      const run = await postJson<GaiaSessionRun>(endpoint, {
        prompt: text,
        resume: resume().trim() || undefined,
      });
      setFeedback({ text: `${kindLabel(run.entry.kind)} session ${run.entry.status}`, kind: "ok" });
      addToast("Gaia session abgeschlossen", "ok", 3500);
      await load();
      await loadStream(run.entry.gaia_session_id);
    } catch (error) {
      const message = error instanceof Error ? error.message : "Gaia session fehlgeschlagen";
      setFeedback({ text: message, kind: "error" });
      addToast(message, "error", 5000);
    } finally {
      setBusy("");
    }
  }

  onMount(() => {
    void load();
    const timer = window.setInterval(() => void load(), 10_000);
    onCleanup(() => window.clearInterval(timer));
  });

  return (
    <section class="col view-panel" data-testid="view-gaia-console">
      <div class="col__head view-head">
        <span>Gaia Console</span>
        <div class="view-toolbar">
          <span class="pill" data-testid="gaia-alert-count">Alerts {alerts().length}</span>
          <span class="pill" data-testid="gaia-session-count">Sessions {sessions().length}</span>
        </div>
      </div>
      <div class="col__body control-shell gaia-console-shell">
        <Show when={feedback()}>
          {(item) => (
            <div data-testid="gaia-feedback" class={`trigger-feedback trigger-feedback--${item().kind}`}>
              {item().text}
            </div>
          )}
        </Show>

        <section class="control-card gaia-session-card">
          <div class="gaia-card-head">
            <h3>Claude Session</h3>
            <button data-testid="gaia-refresh" disabled={busy() !== ""} onClick={() => void load()}>
              Refresh
            </button>
          </div>
          <form
            class="gaia-session-form"
            onSubmit={(event) => {
              event.preventDefault();
              void startSession();
            }}
          >
            <div class="segmented-control">
              <button
                type="button"
                data-testid="gaia-mode-deep"
                class={mode() === "deep" ? "active" : ""}
                onClick={() => setMode("deep")}
              >
                Deep
              </button>
              <button
                type="button"
                data-testid="gaia-mode-setup"
                class={mode() === "setup" ? "active" : ""}
                onClick={() => setMode("setup")}
              >
                Setup
              </button>
            </div>
            <textarea
              data-testid="gaia-prompt"
              rows={5}
              value={prompt()}
              placeholder={mode() === "setup" ? "Setup request" : "Operator task"}
              onInput={(event) => setPrompt(event.currentTarget.value)}
            />
            <div class="gaia-submit-row">
              <input
                data-testid="gaia-resume"
                value={resume()}
                placeholder="resume session id"
                onInput={(event) => setResume(event.currentTarget.value)}
              />
              <button class="primary" data-testid="gaia-start" disabled={!canStart()} type="submit">
                {busy() === "session" ? "Laeuft..." : "Ausfuehren"}
              </button>
            </div>
          </form>
        </section>

        <div class="gaia-console-grid">
          <section class="control-card">
            <h3>Readiness Alerts</h3>
            <Show when={!loading()} fallback={<p class="muted">Lade Alerts...</p>}>
              <Show when={latestAlerts().length > 0} fallback={<p class="muted">Keine Alerts.</p>}>
                <div class="gaia-list" data-testid="gaia-alerts">
                  <For each={latestAlerts()}>
                    {(alert) => (
                      <article class="analysis-item" data-severity={alert.severity}>
                        <div class="analysis-item__head">
                          <strong>{alert.summary}</strong>
                          <span class="pill">{alert.severity}</span>
                        </div>
                        <p>{alert.recommendation}</p>
                        <div class="analysis-item__meta">
                          <span class="mono">{alert.source_event_id}</span>
                          <span>Tick {alert.tick}</span>
                          <span>{timeLabel(alert.timestamp_ms)}</span>
                        </div>
                        <div class="override-list">
                          <For each={alert.unresolved_keys}>{(key) => <span class="pill">{key}</span>}</For>
                        </div>
                      </article>
                    )}
                  </For>
                </div>
              </Show>
            </Show>
          </section>

          <section class="control-card">
            <h3>Sessions</h3>
            <Show when={!loading()} fallback={<p class="muted">Lade Sessions...</p>}>
              <Show when={latestSessions().length > 0} fallback={<p class="muted">Keine Sessions.</p>}>
                <div class="gaia-list" data-testid="gaia-sessions">
                  <For each={latestSessions()}>
                    {(session) => (
                      <button
                        type="button"
                        class={`gaia-session-row ${selectedSessionId() === session.gaia_session_id ? "selected" : ""}`}
                        data-testid="gaia-session-row"
                        onClick={() => void loadStream(session.gaia_session_id)}
                      >
                        <span class="gaia-session-row__main">
                          <strong>{kindLabel(session.kind)}</strong>
                          <span class="mono">{session.gaia_session_id}</span>
                        </span>
                        <span class={statusClass(session.status)}>{session.status}</span>
                        <span class="muted">{totalTokens(session)} tok</span>
                        <span class="muted">{costLabel(session)}</span>
                      </button>
                    )}
                  </For>
                </div>
              </Show>
            </Show>
          </section>
        </div>

        <section class="control-card gaia-stream-card">
          <div class="gaia-card-head">
            <h3>Stream</h3>
            <Show when={selectedSession()}>
              {(session) => (
                <span class="pill mono" data-testid="gaia-selected-session">
                  {session().gaia_session_id}
                </span>
              )}
            </Show>
          </div>
          <pre class="json-block gaia-stream" data-testid="gaia-stream">
            {stream() || (busy() === "stream" ? "Lade Stream..." : "Kein Stream geladen.")}
          </pre>
        </section>
      </div>
    </section>
  );
}
