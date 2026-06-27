import {
  createSignal,
  createMemo,
  For,
  Show,
  onMount,
  onCleanup,
  type JSX,
} from "solid-js";
import {
  apiJson,
  postJson,
  type FsListing,
  type FsEntry,
  type FsFileRead,
  type AgentLifecycleResult,
  type EventsResponse,
} from "../api";
import { selectedAgentId, setSelectedAgentId } from "../state/selection";
import { consoleStore } from "../stores/console";
import { percentValue } from "./format";

// #428 Agent Deep View: a single-agent panel over Sentinel's REAL telemetry — a read-only CAS-FUSE
// FS browser (sentinel-fs), per-agent activity charts from the event log, and per-agent Start/Stop
// (pause/resume, NOT despawn) plus a separate confirm-gated destructive remove. The agent id arrives
// via the shared `selectedAgentId` signal (set by an AgentsView click); consumed + cleared on mount.

const STATUS_LABELS: Record<string, string> = {
  active: "Aktiv",
  suspended: "Pausiert", // #428: canonical status "suspended" is rendered as "Paused"
  errored: "Fehler",
  despawned: "Despawned",
};

const BIO_FIELDS: { key: string; label: string }[] = [
  { key: "hunger", label: "Hunger" },
  { key: "energy", label: "Energie" },
  { key: "stress", label: "Stress" },
  { key: "bladder", label: "Blase" },
  { key: "social_need", label: "Sozial" },
  { key: "caffeine_mg", label: "Koffein" },
];

const DONUT_COLORS = ["#6cf", "#fc6", "#6f9", "#f69", "#9c6", "#c9f", "#999"];

// Deep-link fallback: read the agent id from the URL hash (`#deep=<id>`) when no agent was selected
// via a click — useful for sharing a direct link to one agent's deep view.
function readDeepLinkAgentId(): number | null {
  if (typeof window === "undefined") return null;
  const m = /[#&]deep=(\d+)/.exec(window.location.hash);
  return m ? Number(m[1]) : null;
}

function Sparkline(props: { values: number[] }): JSX.Element {
  const w = 280;
  const h = 48;
  const points = (): string => {
    const vals = props.values;
    if (vals.length === 0) return "";
    const max = Math.max(...vals, 1e-9);
    return vals
      .map((v, i) => {
        const x = vals.length <= 1 ? w : (i / (vals.length - 1)) * w;
        const y = h - (v / max) * h;
        return `${x.toFixed(1)},${y.toFixed(1)}`;
      })
      .join(" ");
  };
  return (
    <svg
      data-testid="deep-sparkline"
      width={w}
      height={h}
      viewBox={`0 0 ${w} ${h}`}
      style={{ overflow: "visible" }}
    >
      <polyline points={points()} fill="none" stroke="var(--accent, #6cf)" stroke-width="2" />
    </svg>
  );
}

function Donut(props: { data: { label: string; value: number }[] }): JSX.Element {
  const r = 52;
  const sw = 22;
  const cx = 70;
  const cy = 70;
  const circumference = 2 * Math.PI * r;
  const total = createMemo(() => props.data.reduce((sum, d) => sum + d.value, 0));
  const segments = createMemo(() => {
    const t = total();
    let offset = 0;
    return props.data.map((d, i) => {
      const frac = t > 0 ? d.value / t : 0;
      const len = frac * circumference;
      const seg = { ...d, frac, len, offset, color: DONUT_COLORS[i % DONUT_COLORS.length] };
      offset += len;
      return seg;
    });
  });
  return (
    <div class="row" style={{ gap: "16px", "align-items": "center", "flex-wrap": "wrap" }}>
      <svg data-testid="deep-donut" width="140" height="140" viewBox="0 0 140 140">
        <Show
          when={total() > 0}
          fallback={
            <circle cx={cx} cy={cy} r={r} fill="none" stroke="var(--border, #333)" stroke-width={sw} />
          }
        >
          <For each={segments()}>
            {(s) => (
              <circle
                cx={cx}
                cy={cy}
                r={r}
                fill="none"
                stroke={s.color}
                stroke-width={sw}
                stroke-dasharray={`${s.len.toFixed(2)} ${(circumference - s.len).toFixed(2)}`}
                stroke-dashoffset={`${(-s.offset).toFixed(2)}`}
                transform={`rotate(-90 ${cx} ${cy})`}
              />
            )}
          </For>
        </Show>
      </svg>
      <div class="col" style={{ gap: "2px", "font-size": "12px" }}>
        <For each={segments()}>
          {(s) => (
            <div
              data-testid="deep-donut-legend"
              class="row"
              style={{ gap: "6px", "align-items": "center" }}
            >
              <span
                aria-hidden="true"
                style={{ width: "10px", height: "10px", background: s.color, "border-radius": "2px" }}
              />
              <span style={{ "font-family": "monospace" }}>{s.label}</span>
              <span class="muted">
                {s.value} ({Math.round(s.frac * 100)}%)
              </span>
            </div>
          )}
        </For>
      </div>
    </div>
  );
}

export function AgentDeepView(): JSX.Element {
  // The agent id comes from the shared selection (an AgentsView click), with a `#deep=<id>` URL-hash
  // fallback for deep-linking. Consume-and-clear the shared signal so a stale click never overrides
  // this panel's agent on a later re-open.
  const initial = selectedAgentId() ?? readDeepLinkAgentId();
  const [agentId] = createSignal<number | null>(initial);
  setSelectedAgentId(null);

  const aggregateId = createMemo(() =>
    agentId() != null ? `AGENT-${String(agentId()).padStart(2, "0")}` : "",
  );
  const agent = createMemo(() => consoleStore.agents.find((a) => a.agent_id === agentId()));

  // FS browser state (inode-based navigation from the agent root, inode 1).
  const [listing, setListing] = createSignal<FsListing | null>(null);
  const [crumbs, setCrumbs] = createSignal<{ name: string; inode: number }[]>([
    { name: "/", inode: 1 },
  ]);
  const [fileView, setFileView] = createSignal<FsFileRead | null>(null);
  const [fsError, setFsError] = createSignal<string>("");

  // Activity state.
  const [events, setEvents] = createSignal<EventsResponse["events"]>([]);

  // Lifecycle state.
  const [lifecycleMsg, setLifecycleMsg] = createSignal<string>("");
  const [confirmRemove, setConfirmRemove] = createSignal(false);

  async function loadDir(inode: number): Promise<void> {
    const id = agentId();
    if (id == null) return;
    try {
      const v = await apiJson<FsListing>(`/api/control/agent/${id}/fs?inode=${inode}`);
      setListing(v);
      setFileView(null);
      setFsError("");
    } catch (e) {
      setFsError(e instanceof Error ? e.message : String(e));
    }
  }

  function enterDir(entry: FsEntry): void {
    setCrumbs([...crumbs(), { name: entry.name, inode: entry.inode }]);
    void loadDir(entry.inode);
  }

  function crumbTo(index: number): void {
    const next = crumbs().slice(0, index + 1);
    setCrumbs(next);
    void loadDir(next[next.length - 1].inode);
  }

  async function openFile(entry: FsEntry): Promise<void> {
    const id = agentId();
    if (id == null) return;
    try {
      setFileView(await apiJson<FsFileRead>(`/api/control/agent/${id}/fs/read?inode=${entry.inode}`));
      setFsError("");
    } catch (e) {
      setFsError(e instanceof Error ? e.message : String(e));
    }
  }

  async function loadActivity(): Promise<void> {
    const agg = aggregateId();
    if (!agg) return;
    try {
      const v = await apiJson<EventsResponse>(`/api/events?agent=${agg}&limit=200`);
      setEvents(v.events ?? []);
    } catch {
      /* keep last good data on a transient read error */
    }
  }

  async function lifecycle(action: "stop" | "start" | "remove"): Promise<void> {
    const id = agentId();
    if (id == null) return;
    try {
      const r = await postJson<AgentLifecycleResult>(`/api/control/agent/${id}/${action}`, {});
      setLifecycleMsg(`${r.action}: ${r.new_status || r.outcome} — ${r.note}`);
    } catch (e) {
      setLifecycleMsg(`Fehler: ${e instanceof Error ? e.message : String(e)}`);
    }
  }

  onMount(() => {
    void loadDir(1);
    void loadActivity();
    const timer = window.setInterval(() => void loadActivity(), 10_000);
    onCleanup(() => window.clearInterval(timer));
  });

  // Activity sparkline: event counts over 24 time buckets (chronological).
  const sparkValues = createMemo(() => {
    const evs = events();
    if (evs.length === 0) return [];
    const sorted = [...evs].sort((a, b) => a.timestamp_ms - b.timestamp_ms);
    const buckets = 24;
    const min = sorted[0].timestamp_ms;
    const max = sorted[sorted.length - 1].timestamp_ms;
    const span = Math.max(1, max - min);
    const counts = new Array(buckets).fill(0) as number[];
    for (const e of sorted) {
      const idx = Math.min(buckets - 1, Math.floor(((e.timestamp_ms - min) / span) * buckets));
      counts[idx] += 1;
    }
    return counts;
  });

  // Tool donut: agent_action_received -> action_type (the tool/action), else event_type.
  const donutData = createMemo(() => {
    const counts = new Map<string, number>();
    for (const e of events()) {
      let category = e.event_type;
      if (e.event_type === "agent_action_received") {
        try {
          const p = JSON.parse(e.payload) as { action_type?: unknown };
          if (typeof p.action_type === "string" && p.action_type) category = p.action_type;
        } catch {
          /* fall back to event_type */
        }
      }
      counts.set(category, (counts.get(category) ?? 0) + 1);
    }
    return [...counts.entries()]
      .map(([label, value]) => ({ label, value }))
      .sort((a, b) => b.value - a.value)
      .slice(0, 6);
  });

  const statusKey = createMemo(() => agent()?.status ?? "active");

  return (
    <div
      data-testid="view-agent-deep"
      class="col"
      style={{ gap: "12px", padding: "12px", overflow: "auto", height: "100%" }}
    >
      <Show
        when={agentId() != null}
        fallback={
          <p class="muted">Kein Agent ausgewaehlt — klicke einen Agent in der Agents-Ansicht.</p>
        }
      >
        <section class="control-card">
          <div class="row" style={{ "justify-content": "space-between", "align-items": "center" }}>
            <div>
              <h3 style={{ margin: "0 0 2px" }} data-testid="deep-agent-name">
                {agent()?.name ?? aggregateId()}
              </h3>
              <p class="muted" style={{ margin: 0, "font-size": "12px" }}>
                {agent()?.role ?? ""} · {aggregateId()}
              </p>
            </div>
            <span
              data-testid="deep-status"
              class={`status-badge status-badge--${statusKey()}`}
            >
              {STATUS_LABELS[statusKey()] ?? statusKey()}
            </span>
          </div>

          <div class="row" style={{ gap: "8px", "margin-top": "8px", "flex-wrap": "wrap" }}>
            <button data-testid="deep-stop" onClick={() => void lifecycle("stop")}>
              Stop (Pause)
            </button>
            <button data-testid="deep-start" onClick={() => void lifecycle("start")}>
              Start (Resume)
            </button>
            <Show
              when={confirmRemove()}
              fallback={
                <button
                  data-testid="deep-remove"
                  class="btn-danger"
                  onClick={() => setConfirmRemove(true)}
                >
                  Agent entfernen…
                </button>
              }
            >
              <span class="muted" style={{ "align-self": "center" }}>
                Endgueltig entfernen (ECS + Sandbox)?
              </span>
              <button
                data-testid="deep-remove-confirm"
                class="btn-danger"
                onClick={() => {
                  setConfirmRemove(false);
                  void lifecycle("remove");
                }}
              >
                Ja, entfernen
              </button>
              <button data-testid="deep-remove-cancel" onClick={() => setConfirmRemove(false)}>
                Abbrechen
              </button>
            </Show>
          </div>
          <Show when={lifecycleMsg()}>
            <p data-testid="deep-lifecycle-msg" class="muted" style={{ "font-size": "12px" }}>
              {lifecycleMsg()}
            </p>
          </Show>

          <div class="bio-section" style={{ "margin-top": "8px" }}>
            <For each={BIO_FIELDS}>
              {(field) => {
                const value = agent()?.[field.key];
                const pct = percentValue(typeof value === "number" ? value : null);
                const level = pct > 70 ? "high" : pct > 40 ? "mid" : "low";
                return (
                  <div class="bio-bar">
                    <span class="bio-bar__label">{field.label}</span>
                    <span class="bio-bar__track" aria-hidden="true">
                      <span class={`bio-bar__fill bio-bar__fill--${level}`} style={{ width: `${pct}%` }} />
                    </span>
                    <span class="bio-bar__value">{pct}%</span>
                  </div>
                );
              }}
            </For>
          </div>
        </section>

        <section class="control-card">
          <h3 style={{ margin: "0 0 8px" }}>Aktivität (echte Event-Daten)</h3>
          <p class="muted" style={{ "margin-top": 0, "font-size": "12px" }}>
            {events().length} Events aus dem Event-Log ({aggregateId()}).
          </p>
          <Sparkline values={sparkValues()} />
          <div style={{ "margin-top": "10px" }}>
            <Donut data={donutData()} />
          </div>
        </section>

        <section class="control-card">
          <h3 style={{ margin: "0 0 8px" }}>Dateisystem (read-only, CAS-FUSE)</h3>
          <div class="row" data-testid="fs-breadcrumb" style={{ gap: "4px", "flex-wrap": "wrap", "font-size": "12px" }}>
            <For each={crumbs()}>
              {(crumb, i) => (
                <button
                  data-testid="fs-crumb"
                  class="link-btn"
                  onClick={() => crumbTo(i())}
                  style={{ "font-family": "monospace" }}
                >
                  {crumb.name}
                  {i() < crumbs().length - 1 ? " /" : ""}
                </button>
              )}
            </For>
          </div>
          <Show when={listing()}>
            {(l) => (
              <p class="muted" style={{ "font-size": "11px", "margin-bottom": "4px" }} data-testid="fs-dedup">
                Dedup: {l().dedup_ratio_percent.toFixed(1)}% gespart ({l().cas_blob_count} CAS-Blobs,{" "}
                {l().dedup_savings_bytes} B)
              </p>
            )}
          </Show>
          <Show when={fsError()}>
            <p class="muted" data-testid="fs-error">
              {fsError()}
            </p>
          </Show>
          <div class="col" style={{ gap: "2px" }}>
            <For each={listing()?.entries ?? []}>
              {(entry) => (
                <button
                  data-testid="fs-entry"
                  class="link-btn"
                  data-kind={entry.kind}
                  onClick={() => (entry.kind === "dir" ? enterDir(entry) : void openFile(entry))}
                  style={{
                    display: "grid",
                    "grid-template-columns": "1fr 80px 70px 70px",
                    gap: "8px",
                    "font-size": "12px",
                    "text-align": "left",
                  }}
                >
                  <span style={{ "font-family": "monospace" }}>
                    {entry.kind === "dir" ? "📁" : "📄"} {entry.name}
                  </span>
                  <span class="muted">{entry.size} B</span>
                  <span class="muted">{entry.kind}</span>
                  <span class="muted" data-testid="fs-entry-refcount">
                    ×{entry.refcount}
                  </span>
                </button>
              )}
            </For>
          </div>
          <Show when={fileView()}>
            {(f) => (
              <div style={{ "margin-top": "8px" }}>
                <p class="muted" style={{ "font-size": "11px" }}>
                  {f().size} B · {f().encoding}
                  {f().truncated ? " · abgeschnitten" : ""} · ×{f().refcount} geteilt
                </p>
                <pre
                  data-testid="fs-file-content"
                  style={{
                    "max-height": "240px",
                    overflow: "auto",
                    background: "var(--panel, #111)",
                    padding: "8px",
                    "font-size": "12px",
                    "white-space": "pre-wrap",
                    "word-break": "break-all",
                  }}
                >
                  {f().content}
                </pre>
              </div>
            )}
          </Show>
        </section>
      </Show>
    </div>
  );
}
