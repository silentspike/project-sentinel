import { createEffect, createMemo, createSignal, For, onMount, Show, type JSX } from "solid-js";
import { createStore, reconcile } from "solid-js/store";
import { apiJson, putJson, type BuildingConfig, type DaemonParams, type RoomType } from "../api";
import { addToast } from "../components/controls";
import { validateAdjacency } from "./config/validation";

// #423 Config Editor — two tabs:
//  - Rooms: fully editable; adjacency is auto-mirrored (toggle a↔b adds/removes both ways) so the
//    user can't create the one-sided adjacency that BuildingConfig::validate rejects. Saves via
//    PUT /api/config/rooms -> daemon apply (#425, sole writer).
//  - Daemon: READ-ONLY viewer of max_agents/time_scale/tick_rate_ms. daemon.toml has no apply path
//    (OperatorConfigApplyCommand carries only agents+building) and the net-exposed dashboard must not
//    write config_dir (#420/#474). Editing these = a follow-up needing a daemon operator endpoint.

const ROOM_TYPES: RoomType[] = ["office", "meeting", "common", "break", "transit", "bathroom"];

const emptyBuilding = (): BuildingConfig => ({ building: { name: "", address: "", floors: 1 }, rooms: [] });

export function ConfigEditorView(): JSX.Element {
  const [tab, setTab] = createSignal<"rooms" | "daemon">("rooms");
  const [loaded, setLoaded] = createSignal<BuildingConfig | null>(null);
  const [building, setBuilding] = createStore<BuildingConfig>(emptyBuilding());
  const [daemon, setDaemon] = createSignal<DaemonParams | null>(null);
  const [busy, setBusy] = createSignal(false);

  async function load(): Promise<void> {
    try {
      const b = await apiJson<BuildingConfig>("/api/config/rooms");
      setLoaded(b);
      setBuilding(reconcile(structuredClone(b)));
    } catch (err) {
      addToast(err instanceof Error ? err.message : "rooms laden fehlgeschlagen", "error", 5000);
    }
    try {
      setDaemon(await apiJson<DaemonParams>("/api/config/daemon"));
    } catch {
      // daemon viewer is best-effort
    }
  }
  onMount(() => void load());
  // keep `loaded` referenced for the dirty memo even if rooms reload
  createEffect(() => void loaded());

  const adjErrors = createMemo(() => validateAdjacency(building));
  const dirty = createMemo(
    () => loaded() != null && JSON.stringify(building) !== JSON.stringify(loaded()),
  );

  // Toggle a↔b adjacency on BOTH rooms (auto-mirror) so it is always bidirectional.
  function toggleAdjacency(a: string, b: string, on: boolean): void {
    const upd = (other: string) => (adj: string[]): string[] =>
      on ? Array.from(new Set([...adj, other])) : adj.filter((x) => x !== other);
    setBuilding("rooms", (r) => r.id === a, "adjacent", upd(b));
    setBuilding("rooms", (r) => r.id === b, "adjacent", upd(a));
  }

  async function save(): Promise<void> {
    if (adjErrors().length > 0) return;
    setBusy(true);
    try {
      await putJson("/api/config/rooms", building);
      addToast("rooms.toml gespeichert (Daemon-Apply #425)", "ok", 4000);
      await load();
    } catch (err) {
      addToast(err instanceof Error ? err.message : "Speichern fehlgeschlagen", "error", 5000);
    } finally {
      setBusy(false);
    }
  }

  return (
    <section class="col view-panel" data-testid="view-config-editor">
      <div class="col__head view-head">
        <span>Config Editor</span>
        <div style={{ display: "flex", gap: "6px" }}>
          <button data-testid="ce-tab-rooms" classList={{ primary: tab() === "rooms" }} onClick={() => setTab("rooms")}>
            rooms
          </button>
          <button data-testid="ce-tab-daemon" classList={{ primary: tab() === "daemon" }} onClick={() => setTab("daemon")}>
            daemon
          </button>
        </div>
      </div>

      <div class="col__body" style={{ display: "grid", gap: "10px" }}>
        <Show when={tab() === "rooms"}>
          <fieldset style={{ border: "1px solid var(--border)", "border-radius": "6px", padding: "8px" }}>
            <legend>Building</legend>
            <label>
              Name
              <input value={building.building.name} onInput={(ev) => setBuilding("building", "name", ev.currentTarget.value)} />
            </label>
            <label>
              Adresse
              <input value={building.building.address} onInput={(ev) => setBuilding("building", "address", ev.currentTarget.value)} />
            </label>
            <label>
              Floors
              <input
                type="number"
                min="1"
                value={building.building.floors}
                onInput={(ev) => setBuilding("building", "floors", Number(ev.currentTarget.value))}
              />
            </label>
          </fieldset>

          <Show when={adjErrors().length > 0}>
            <p data-testid="ce-adj-errors" style={{ color: "var(--danger)", "font-size": "13px" }}>
              {adjErrors().join("; ")}
            </p>
          </Show>

          <For each={building.rooms}>
            {(room, i) => (
              <fieldset style={{ border: "1px solid var(--border)", "border-radius": "6px", padding: "8px" }}>
                <legend>{room.id}</legend>
                <label>
                  Name
                  <input value={room.name} onInput={(ev) => setBuilding("rooms", i(), "name", ev.currentTarget.value)} />
                </label>
                <div style={{ display: "flex", gap: "8px" }}>
                  <label>
                    Floor
                    <input
                      type="number"
                      style={{ width: "70px" }}
                      value={room.floor}
                      onInput={(ev) => setBuilding("rooms", i(), "floor", Number(ev.currentTarget.value))}
                    />
                  </label>
                  <label>
                    Capacity
                    <input
                      type="number"
                      min="0"
                      style={{ width: "80px" }}
                      value={room.capacity}
                      onInput={(ev) => setBuilding("rooms", i(), "capacity", Number(ev.currentTarget.value))}
                    />
                  </label>
                  <label>
                    Typ
                    <select
                      value={room.room_type}
                      onChange={(ev) => setBuilding("rooms", i(), "room_type", ev.currentTarget.value as RoomType)}
                    >
                      <For each={ROOM_TYPES}>{(t) => <option value={t}>{t}</option>}</For>
                    </select>
                  </label>
                </div>
                <div style={{ display: "flex", gap: "12px", "margin-top": "4px" }}>
                  <label class="toggle-row">
                    <input
                      type="checkbox"
                      checked={room.has_coffee_machine}
                      onChange={(ev) => setBuilding("rooms", i(), "has_coffee_machine", ev.currentTarget.checked)}
                    />
                    Kaffee
                  </label>
                  <label class="toggle-row">
                    <input
                      type="checkbox"
                      checked={room.has_printer}
                      onChange={(ev) => setBuilding("rooms", i(), "has_printer", ev.currentTarget.checked)}
                    />
                    Drucker
                  </label>
                </div>
                <details>
                  <summary class="muted" style={{ "font-size": "12px" }}>
                    Adjazenz ({room.adjacent.length}) — Auto-Mirror
                  </summary>
                  <div style={{ display: "flex", "flex-wrap": "wrap", gap: "8px", "margin-top": "4px" }}>
                    <For each={building.rooms.filter((o) => o.id !== room.id)}>
                      {(other) => (
                        <label class="toggle-row" style={{ "font-size": "12px" }}>
                          <input
                            type="checkbox"
                            checked={room.adjacent.includes(other.id)}
                            onChange={(ev) => toggleAdjacency(room.id, other.id, ev.currentTarget.checked)}
                          />
                          {other.id}
                        </label>
                      )}
                    </For>
                  </div>
                </details>
              </fieldset>
            )}
          </For>

          <button
            class="primary"
            data-testid="ce-save"
            disabled={busy() || !dirty() || adjErrors().length > 0}
            onClick={() => void save()}
          >
            rooms speichern
          </button>
        </Show>

        <Show when={tab() === "daemon"}>
          <div data-testid="ce-daemon" style={{ display: "grid", gap: "8px" }}>
            <p class="muted">
              Read-only. daemon.toml hat keinen Apply-Pfad — Editieren folgt via Daemon-Operator-Endpunkt
              (Follow-up). Diese Werte greifen erst nach einem Daemon-Neustart.
            </p>
            <div class="control-kv">
              <span>max_agents</span>
              <strong class="mono" data-testid="ce-daemon-max-agents">
                {daemon()?.max_agents ?? "—"}
              </strong>
            </div>
            <div class="control-kv">
              <span>time_scale</span>
              <strong class="mono">{daemon()?.time_scale ?? "—"}</strong>
            </div>
            <div class="control-kv">
              <span>tick_rate_ms</span>
              <strong class="mono">{daemon()?.tick_rate_ms ?? "—"}</strong>
            </div>
          </div>
        </Show>
      </div>
    </section>
  );
}
