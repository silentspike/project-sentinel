import { createEffect, createMemo, createSignal, For, onMount, Show, type JSX } from "solid-js";
import { apiJson, postJson, type SnapshotInfo, type SnapshotWorldState } from "../api";
import { formatBytes, formatDateTime, formatNumber } from "./format";

const TIER_ORDER = ["live", "hourly", "daily", "weekly", "monthly"];

function sortedSnapshots(snapshots: readonly SnapshotInfo[], desc = false): SnapshotInfo[] {
  return snapshots.slice().sort((a, b) => desc ? b.created_at_ms - a.created_at_ms : a.created_at_ms - b.created_at_ms);
}

function simHour(value: unknown): string {
  return typeof value === "number" ? `${value.toFixed(1)}h` : "--";
}

function tierClass(tier: string): string {
  return `tier-${tier || "live"}`;
}

export function TimeTravelView(): JSX.Element {
  const [snapshots, setSnapshots] = createSignal<SnapshotInfo[]>([]);
  const [selectedId, setSelectedId] = createSignal<string | null>(null);
  const [state, setState] = createSignal<SnapshotWorldState | null>(null);
  const [loading, setLoading] = createSignal(true);
  const [stateLoading, setStateLoading] = createSignal(false);
  const [loadError, setLoadError] = createSignal<string | null>(null);
  const [feedback, setFeedback] = createSignal<{ text: string; kind: "ok" | "error" } | null>(null);
  const [busy, setBusy] = createSignal<string | null>(null);

  const asc = createMemo(() => sortedSnapshots(snapshots()));
  const desc = createMemo(() => sortedSnapshots(snapshots(), true));
  const selected = createMemo(() => snapshots().find((snap) => snap.id === selectedId()) ?? null);

  async function loadSnapshots(): Promise<void> {
    setLoading(true);
    setLoadError(null);
    try {
      const rows = await apiJson<SnapshotInfo[]>("/api/control/snapshots");
      const list = Array.isArray(rows) ? rows : [];
      setSnapshots(list);
      if (list.length > 0 && (!selectedId() || !list.some((snap) => snap.id === selectedId()))) {
        setSelectedId(sortedSnapshots(list, true)[0].id);
      }
      if (list.length === 0) setSelectedId(null);
    } catch (error) {
      setSnapshots([]);
      setSelectedId(null);
      setLoadError(error instanceof Error ? error.message : "Snapshots konnten nicht geladen werden");
    } finally {
      setLoading(false);
    }
  }

  async function loadState(snapshotId: string): Promise<void> {
    setStateLoading(true);
    try {
      const result = await apiJson<SnapshotWorldState>(`/api/control/snapshot-state?snapshot_id=${encodeURIComponent(snapshotId)}`);
      if (selectedId() === snapshotId) setState(result);
    } catch (error) {
      if (selectedId() === snapshotId) {
        setState(null);
        setFeedback({ text: error instanceof Error ? error.message : "Snapshot-State nicht verfuegbar", kind: "error" });
      }
    } finally {
      if (selectedId() === snapshotId) setStateLoading(false);
    }
  }

  async function runAction(label: string, fn: () => Promise<unknown>): Promise<void> {
    setBusy(label);
    setFeedback(null);
    try {
      await fn();
      setFeedback({ text: `${label} ausgefuehrt`, kind: "ok" });
      await loadSnapshots();
    } catch (error) {
      setFeedback({ text: error instanceof Error ? error.message : `${label} fehlgeschlagen`, kind: "error" });
    } finally {
      setBusy(null);
    }
  }

  function restore(): void {
    const snap = selected();
    if (!snap) return;
    const ok = window.confirm(`Simulation auf Snapshot ${snap.id} bei Tick ${snap.tick} zuruecksetzen?`);
    if (!ok) return;
    void runAction("Restore", () => postJson("/api/control/snapshot-restore", { snapshot_id: snap.id }));
  }

  createEffect(() => {
    const id = selectedId();
    setState(null);
    if (id) void loadState(id);
  });

  onMount(() => void loadSnapshots());

  return (
    <section class="col view-panel" data-testid="view-timetravel">
      <div class="col__head view-head">
        <span>Time Machine</span>
        <span class="pill" data-testid="snapshot-count">{snapshots().length} Snapshots</span>
      </div>
      <div class="col__body timetravel-shell">
        <Show when={feedback()}>
          {(item) => <div class={`trigger-feedback trigger-feedback--${item().kind}`}>{item().text}</div>}
        </Show>

        <div class="view-toolbar">
          <button data-testid="snapshot-create" disabled={busy() !== null} onClick={() => void runAction("Snapshot", () => postJson("/api/control/snapshot", {}))}>
            Snapshot erstellen
          </button>
          <button data-testid="snapshot-refresh" disabled={busy() !== null} onClick={() => void loadSnapshots()}>
            Aktualisieren
          </button>
          <button class="primary" data-testid="snapshot-restore" disabled={!selected() || busy() !== null} onClick={restore}>
            Restore
          </button>
        </div>

        <Show when={!loading()} fallback={<p class="muted">Lade Snapshots...</p>}>
          <Show
            when={snapshots().length > 0}
            fallback={<div class="degraded-panel">{loadError() ?? "Keine Snapshots vorhanden"}</div>}
          >
            <section class="tt-rail-panel">
              <div class="tt-axis">
                <span>{formatDateTime(asc()[0]?.created_at_ms)}</span>
                <span>{formatDateTime(asc().at(-1)?.created_at_ms)}</span>
              </div>
              <div class="tt-rail">
                <For each={asc()}>
                  {(snap, index) => {
                    const first = asc()[0]?.created_at_ms ?? 0;
                    const last = asc().at(-1)?.created_at_ms ?? first;
                    const span = Math.max(1, last - first);
                    const pct = asc().length > 1
                      ? ((snap.created_at_ms - first) / span) * 100
                      : 50;
                    return (
                      <button
                        class={`tt-marker ${tierClass(snap.tier)} ${selectedId() === snap.id ? "selected" : ""}`}
                        style={{ left: `calc(${pct.toFixed(2)}% - 7px)` }}
                        title={`${snap.tier} | Tick ${snap.tick} | ${formatDateTime(snap.created_at_ms)}`}
                        data-testid={`snapshot-marker-${index()}`}
                        onClick={() => setSelectedId(snap.id)}
                      />
                    );
                  }}
                </For>
              </div>
              <div class="tt-legend">
                <For each={TIER_ORDER.filter((tier) => snapshots().some((snap) => snap.tier === tier))}>
                  {(tier) => <span><i class={`tt-dot ${tierClass(tier)}`} />{tier}</span>}
                </For>
              </div>
            </section>

            <div class="tt-layout">
              <section class="control-card">
                <h3>Snapshots</h3>
                <div class="mini-table mini-table--snapshots">
                  <div class="mini-table__head">Tier</div>
                  <div class="mini-table__head">Zeit</div>
                  <div class="mini-table__head">Tick</div>
                  <div class="mini-table__head">Groesse</div>
                  <For each={desc()}>
                    {(snap) => (
                      <>
                        <button class={`table-button ${selectedId() === snap.id ? "selected" : ""}`} onClick={() => setSelectedId(snap.id)}>
                          <span class={`tier-badge ${tierClass(snap.tier)}`}>{snap.tier}</span>
                        </button>
                        <button class={`table-button ${selectedId() === snap.id ? "selected" : ""}`} onClick={() => setSelectedId(snap.id)}>
                          {formatDateTime(snap.created_at_ms)}
                        </button>
                        <button class={`table-button ${selectedId() === snap.id ? "selected" : ""}`} onClick={() => setSelectedId(snap.id)}>
                          {formatNumber(snap.tick)}
                        </button>
                        <button class={`table-button ${selectedId() === snap.id ? "selected" : ""}`} onClick={() => setSelectedId(snap.id)}>
                          {formatBytes(snap.payload_size_bytes)}
                        </button>
                      </>
                    )}
                  </For>
                </div>
              </section>

              <section class="control-card" data-testid="snapshot-detail">
                <h3>Welt-Zustand</h3>
                <Show when={selected()} fallback={<p class="muted">Snapshot auswaehlen.</p>}>
                  {(snap) => (
                    <>
                      <div class="snapshot-headline">
                        <span class={`tier-badge ${tierClass(snap().tier)}`}>{snap().tier}</span>
                        <strong>Tick {formatNumber(snap().tick)}</strong>
                        <span>{formatDateTime(snap().created_at_ms)}</span>
                      </div>
                      <Show when={!stateLoading()} fallback={<p class="muted">Lade Welt-Zustand...</p>}>
                        <Show when={state()} fallback={<div class="degraded-panel">Snapshot-State nicht verfuegbar</div>}>
                          {(world) => (
                            <>
                              <div class="snapshot-stats">
                                <div><strong>{world().active_agent_count ?? world().present_agent_count}</strong><span>aktive Agents</span></div>
                                <div><strong>{world().present_agent_count}</strong><span>im Gebaeude</span></div>
                                <div><strong>{world().room_count}</strong><span>belegte Raeume</span></div>
                                <div><strong>{simHour(world().sim_hour)}</strong><span>Sim Hour</span></div>
                              </div>
                              <p class="muted">
                                Last Event ID: {world().last_event_id ?? "--"} · Snapshot ID: {world().snapshot_id}
                              </p>
                              <div class="mini-table mini-table--rooms">
                                <div class="mini-table__head">Raum</div>
                                <div class="mini-table__head">Agents</div>
                                <For each={world().rooms ?? []}>
                                  {(room) => (
                                    <>
                                      <div>{room.name}</div>
                                      <div>{room.occupant_count}</div>
                                    </>
                                  )}
                                </For>
                              </div>
                            </>
                          )}
                        </Show>
                      </Show>
                    </>
                  )}
                </Show>
              </section>
            </div>
          </Show>
        </Show>
      </div>
    </section>
  );
}
