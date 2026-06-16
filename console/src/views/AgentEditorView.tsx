import { createEffect, createMemo, createSignal, For, onMount, Show, type JSX } from "solid-js";
import { createStore, reconcile } from "solid-js/store";
import { apiJson, putJson, type AgentConfig } from "../api";
import { addToast, SearchFilter } from "../components/controls";
import { validateAgentRequired, validatePersonality } from "./config/validation";

// #422 Agent Editor — load an existing agent, edit its config, save via PUT /api/config/agents/{id}
// (the backend assembles the full mode:live apply and proxies it to the daemon, the sole writer).
// runtime.nano_runtime stays read-only until #395 ships the explicit tier schema.

const BIG_FIVE = [
  "openness",
  "conscientiousness",
  "extraversion",
  "agreeableness",
  "neuroticism",
  "caffeine_tolerance",
] as const;

const csv = (xs: string[]): string => xs.join(", ");
const fromCsv = (s: string): string[] =>
  s
    .split(",")
    .map((x) => x.trim())
    .filter((x) => x.length > 0);

/** A typed blank agent so the editable store is always a full AgentConfig (form shown only when loaded). */
function blankAgent(): AgentConfig {
  return {
    identity: { id: 0, name: "", role: "", department: "", shift_set: 1, kpis: [], reports_to: null, direct_reports: [] },
    personality: {
      openness: 0.5,
      conscientiousness: 0.5,
      extraversion: 0.5,
      agreeableness: 0.5,
      neuroticism: 0.5,
      caffeine_tolerance: 0.5,
      morning_person: false,
    },
    preferences: { favorite_room: "", coffee_preference: "", lunch_time: "" },
    background: { bio: "", quirks: [] },
    runtime: { nano_runtime: null },
    capabilities: { tools: [], sandbox_allowed_paths: [] },
  };
}

export function AgentEditorView(): JSX.Element {
  const [agents, setAgents] = createSignal<AgentConfig[]>([]);
  const [selectedId, setSelectedId] = createSignal<number | null>(null);
  const [filter, setFilter] = createSignal("");
  const [edited, setEdited] = createStore<AgentConfig>(blankAgent());
  const [busy, setBusy] = createSignal(false);

  const original = createMemo(() => agents().find((a) => a.identity.id === selectedId()) ?? null);
  const isLoaded = (): boolean => selectedId() !== null;
  const e = (): AgentConfig => edited;

  const filtered = createMemo(() => {
    const q = filter().toLowerCase();
    return agents().filter(
      (a) => !q || a.identity.name.toLowerCase().includes(q) || a.identity.role.toLowerCase().includes(q),
    );
  });

  async function load(): Promise<void> {
    try {
      setAgents(await apiJson<AgentConfig[]>("/api/config/agents"));
    } catch (err) {
      addToast(err instanceof Error ? err.message : "Agents laden fehlgeschlagen", "error", 5000);
    }
  }
  onMount(() => void load());

  // When the selection changes, copy the original into the editable store.
  createEffect(() => {
    const o = original();
    if (o) setEdited(reconcile(structuredClone(o)));
  });

  const errors = createMemo<string[]>(() =>
    isLoaded() ? [...validatePersonality(e().personality), ...validateAgentRequired(e())] : ["no agent"],
  );
  const dirty = createMemo(
    () => isLoaded() && original() != null && JSON.stringify(e()) !== JSON.stringify(original()),
  );
  // Field-level dirty highlight: orange border when the field differs from the loaded value.
  const fieldStyle = (changed: boolean): JSX.CSSProperties =>
    changed ? { "border-color": "var(--warn)" } : {};

  async function save(): Promise<void> {
    if (!isLoaded() || errors().length > 0) return;
    setBusy(true);
    try {
      await putJson(`/api/config/agents/${e().identity.id}`, e());
      addToast(`Agent ${e().identity.name} gespeichert (Daemon-Apply #425)`, "ok", 4000);
      await load();
    } catch (err) {
      addToast(err instanceof Error ? err.message : "Speichern fehlgeschlagen", "error", 5000);
    } finally {
      setBusy(false);
    }
  }

  return (
    <section class="col view-panel" data-testid="view-agent-editor">
      <div class="col__head view-head">
        <span>Agent Editor</span>
        <Show when={dirty()}>
          <span class="pill pill-warn">geaendert</span>
        </Show>
      </div>
      <div class="col__body" style={{ display: "grid", gap: "10px" }}>
        <SearchFilter placeholder="Agent suchen (Name/Rolle)…" onFilter={setFilter} />
        <select
          data-testid="ae-select"
          value={selectedId() ?? ""}
          onChange={(ev) => setSelectedId(ev.currentTarget.value === "" ? null : Number(ev.currentTarget.value))}
        >
          <option value="">— Agent waehlen —</option>
          <For each={filtered()}>
            {(a) => (
              <option value={a.identity.id}>
                {a.identity.name} · {a.identity.role}
              </option>
            )}
          </For>
        </select>

        <Show when={isLoaded()} fallback={<p class="muted">Waehle einen Agenten zum Bearbeiten.</p>}>
          <fieldset style={{ border: "1px solid var(--border)", "border-radius": "6px", padding: "8px" }}>
            <legend>Identity (id {e().identity.id} — read-only)</legend>
            <label>
              Name
              <input
                data-testid="ae-name"
                style={fieldStyle(e().identity.name !== original()?.identity.name)}
                value={e().identity.name}
                onInput={(ev) => setEdited("identity", "name", ev.currentTarget.value)}
              />
            </label>
            <label>
              Rolle
              <input value={e().identity.role} onInput={(ev) => setEdited("identity", "role", ev.currentTarget.value)} />
            </label>
            <label>
              Department
              <input
                value={e().identity.department}
                onInput={(ev) => setEdited("identity", "department", ev.currentTarget.value)}
              />
            </label>
            <label>
              shift_set
              <input
                type="number"
                value={e().identity.shift_set}
                onInput={(ev) => setEdited("identity", "shift_set", Number(ev.currentTarget.value))}
              />
            </label>
            <label>
              KPIs (Komma-getrennt)
              <input value={csv(e().identity.kpis)} onInput={(ev) => setEdited("identity", "kpis", fromCsv(ev.currentTarget.value))} />
            </label>
          </fieldset>

          <fieldset style={{ border: "1px solid var(--border)", "border-radius": "6px", padding: "8px" }}>
            <legend>Personality (Big Five, [0..1])</legend>
            <For each={BIG_FIVE}>
              {(trait) => (
                <label class="range-row">
                  {trait} <strong>{e().personality[trait].toFixed(2)}</strong>
                  <input
                    type="range"
                    min="0"
                    max="1"
                    step="0.05"
                    value={e().personality[trait]}
                    onInput={(ev) => setEdited("personality", trait, Number(ev.currentTarget.value))}
                  />
                </label>
              )}
            </For>
            <label class="toggle-row">
              <input
                type="checkbox"
                checked={e().personality.morning_person}
                onChange={(ev) => setEdited("personality", "morning_person", ev.currentTarget.checked)}
              />
              morning_person
            </label>
          </fieldset>

          <fieldset style={{ border: "1px solid var(--border)", "border-radius": "6px", padding: "8px" }}>
            <legend>Preferences / Background</legend>
            <label>
              favorite_room
              <input
                value={e().preferences.favorite_room}
                onInput={(ev) => setEdited("preferences", "favorite_room", ev.currentTarget.value)}
              />
            </label>
            <label>
              coffee_preference
              <input
                value={e().preferences.coffee_preference}
                onInput={(ev) => setEdited("preferences", "coffee_preference", ev.currentTarget.value)}
              />
            </label>
            <label>
              lunch_time
              <input value={e().preferences.lunch_time} onInput={(ev) => setEdited("preferences", "lunch_time", ev.currentTarget.value)} />
            </label>
            <label>
              Bio
              <textarea rows={3} value={e().background.bio} onInput={(ev) => setEdited("background", "bio", ev.currentTarget.value)} />
            </label>
            <label>
              Quirks (Komma-getrennt)
              <input value={csv(e().background.quirks)} onInput={(ev) => setEdited("background", "quirks", fromCsv(ev.currentTarget.value))} />
            </label>
          </fieldset>

          <fieldset style={{ border: "1px solid var(--border)", "border-radius": "6px", padding: "8px" }}>
            <legend>Capabilities / Runtime</legend>
            <label>
              tools (Komma-getrennt)
              <input value={csv(e().capabilities.tools)} onInput={(ev) => setEdited("capabilities", "tools", fromCsv(ev.currentTarget.value))} />
            </label>
            <label>
              sandbox_allowed_paths (Komma-getrennt)
              <input
                value={csv(e().capabilities.sandbox_allowed_paths)}
                onInput={(ev) => setEdited("capabilities", "sandbox_allowed_paths", fromCsv(ev.currentTarget.value))}
              />
            </label>
            <label>
              nano_runtime (read-only bis #395)
              <input data-testid="ae-nano-runtime" readOnly value={e().runtime.nano_runtime ?? "—"} />
            </label>
          </fieldset>

          <Show when={errors().length > 0}>
            <p data-testid="ae-errors" style={{ color: "var(--danger)", "font-size": "13px" }}>
              {errors().join("; ")}
            </p>
          </Show>
          <button
            class="primary"
            data-testid="ae-save"
            disabled={busy() || !dirty() || errors().length > 0}
            onClick={() => void save()}
          >
            Speichern
          </button>
        </Show>
      </div>
    </section>
  );
}
