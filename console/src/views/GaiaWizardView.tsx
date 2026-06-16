import { createMemo, createSignal, For, Show, type JSX } from "solid-js";
import { createStore } from "solid-js/store";
import { postJson, type GaiaSpec, type GeneratePreview } from "../api";
import { addToast } from "../components/controls";
import { validateGaiaStep } from "./config/validation";

// #421 Gaia Web Wizard — describe a company in steps, preview via the deterministic generator
// (POST /api/config/generate, no LLM), then deploy via POST /api/config/apply {mode:"fresh"}.
//
// ⚠️ The final deploy is mode:"fresh" = it REPLACES the entire running company. Destructive on a
// live VM; the backend proxies it to the daemon (#425, sole writer).

const COMPANY_TYPES = ["software_agency", "manufacturing", "healthcare", "generic"] as const;
const SHIFT_MODELS = ["office_hours", "three_shift", "hybrid"] as const;
const CULTURE_AXES = ["formality", "collaboration", "conflict_level", "innovation", "diversity"] as const;
const STEP_TITLES = ["Company", "Shift & Time", "Departments", "Culture", "Preview", "Done"];

function defaultSpec(): GaiaSpec {
  return {
    company_name: "",
    company_type: "software_agency",
    city: "Nuernberg",
    address: "Fuerther Strasse 42, 90429 Nuernberg",
    agent_count: 26,
    seed: 42,
    shift_model: "hybrid",
    time_scale: 1.0,
    departments: [],
    culture: {
      formality: 0.5,
      collaboration: 0.5,
      conflict_level: 0.5,
      innovation: 0.5,
      diversity: 0.5,
      mission: "",
      values: [],
    },
  };
}

export function GaiaWizardView(): JSX.Element {
  const [spec, setSpec] = createStore<GaiaSpec>(defaultSpec());
  const [step, setStep] = createSignal(1);
  const [preview, setPreview] = createSignal<GeneratePreview | null>(null);
  const [busy, setBusy] = createSignal(false);
  const [confirmOverwrite, setConfirmOverwrite] = createSignal(false);

  const stepValid = createMemo(() => validateGaiaStep(step(), spec));

  async function runGenerate(): Promise<void> {
    setBusy(true);
    try {
      const p = await postJson<GeneratePreview>("/api/config/generate", spec);
      setPreview(p);
      setStep(5);
    } catch (e) {
      addToast(e instanceof Error ? e.message : "Generierung fehlgeschlagen", "error", 5000);
    } finally {
      setBusy(false);
    }
  }

  async function runDeploy(): Promise<void> {
    const p = preview();
    if (!p || !confirmOverwrite()) return;
    setBusy(true);
    try {
      // mode:"fresh" ERSETZT die gesamte laufende Firma — destruktiv (Backend → Daemon #425).
      await postJson("/api/config/apply", { mode: "fresh", agents: p.agents, building: p.building });
      addToast("Firma deployed — der Daemon laedt sie nach dem Apply", "ok", 5000);
      setStep(6);
    } catch (e) {
      addToast(e instanceof Error ? e.message : "Deploy fehlgeschlagen", "error", 5000);
    } finally {
      setBusy(false);
    }
  }

  function next(): void {
    if (step() === 4) {
      void runGenerate(); // Culture → generate → Preview(5)
      return;
    }
    if (stepValid()) setStep((s) => Math.min(s + 1, 6));
  }
  const back = (): void => {
    setStep((s) => Math.max(s - 1, 1));
  };

  const addDept = (): void =>
    setSpec("departments", (d) => [...d, { name: "", weight: 1, roles: [] }]);
  const removeDept = (i: number): void =>
    setSpec("departments", (d) => d.filter((_, idx) => idx !== i));

  return (
    <section class="col view-panel" data-testid="view-gaia-wizard">
      <div class="col__head view-head">
        <span>Gaia Wizard</span>
        <span class="pill">
          Schritt {step()}/6 — {STEP_TITLES[step() - 1]}
        </span>
      </div>
      <div class="col__body" style={{ display: "grid", gap: "12px" }}>
        <Show when={step() === 1}>
          <label>
            Firmenname
            <input
              data-testid="gw-company-name"
              value={spec.company_name}
              onInput={(e) => setSpec("company_name", e.currentTarget.value)}
              style={{ width: "100%" }}
            />
          </label>
          <label>
            Typ
            <select
              value={spec.company_type}
              onChange={(e) => setSpec("company_type", e.currentTarget.value as GaiaSpec["company_type"])}
            >
              <For each={COMPANY_TYPES}>{(t) => <option value={t}>{t}</option>}</For>
            </select>
          </label>
          <label>
            Stadt
            <input value={spec.city} onInput={(e) => setSpec("city", e.currentTarget.value)} style={{ width: "100%" }} />
          </label>
          <label>
            Adresse
            <input value={spec.address} onInput={(e) => setSpec("address", e.currentTarget.value)} style={{ width: "100%" }} />
          </label>
          <label>
            Agent-Anzahl
            <input
              data-testid="gw-agent-count"
              type="number"
              min="1"
              value={spec.agent_count}
              onInput={(e) => setSpec("agent_count", Number(e.currentTarget.value))}
            />
          </label>
          <label>
            Seed
            <input type="number" value={spec.seed} onInput={(e) => setSpec("seed", Number(e.currentTarget.value))} />
          </label>
        </Show>

        <Show when={step() === 2}>
          <label>
            Schichtmodell
            <select
              value={spec.shift_model}
              onChange={(e) => setSpec("shift_model", e.currentTarget.value as GaiaSpec["shift_model"])}
            >
              <For each={SHIFT_MODELS}>{(s) => <option value={s}>{s}</option>}</For>
            </select>
          </label>
          <label>
            time_scale (&gt; 0)
            <input
              data-testid="gw-time-scale"
              type="number"
              step="0.1"
              min="0.1"
              value={spec.time_scale}
              onInput={(e) => setSpec("time_scale", Number(e.currentTarget.value))}
            />
          </label>
        </Show>

        <Show when={step() === 3}>
          <p class="muted">Optional — leer lassen → der Generator leitet Abteilungen aus dem Typ ab.</p>
          <For each={spec.departments}>
            {(d, i) => (
              <div style={{ display: "flex", gap: "6px" }}>
                <input
                  placeholder="Name"
                  value={d.name}
                  onInput={(e) => setSpec("departments", i(), "name", e.currentTarget.value)}
                />
                <input
                  type="number"
                  min="1"
                  style={{ width: "70px" }}
                  value={d.weight}
                  onInput={(e) => setSpec("departments", i(), "weight", Number(e.currentTarget.value))}
                />
                <button onClick={() => removeDept(i())}>✕</button>
              </div>
            )}
          </For>
          <button data-testid="gw-add-dept" onClick={addDept}>
            + Abteilung
          </button>
        </Show>

        <Show when={step() === 4}>
          <For each={CULTURE_AXES}>
            {(axis) => (
              <label class="range-row">
                {axis} <strong>{spec.culture[axis].toFixed(2)}</strong>
                <input
                  type="range"
                  min="0"
                  max="1"
                  step="0.05"
                  value={spec.culture[axis]}
                  onInput={(e) => setSpec("culture", axis, Number(e.currentTarget.value))}
                />
              </label>
            )}
          </For>
          <label>
            Mission
            <textarea
              rows={2}
              value={spec.culture.mission}
              onInput={(e) => setSpec("culture", "mission", e.currentTarget.value)}
            />
          </label>
        </Show>

        <Show when={step() === 5 && preview()}>
          {(_) => {
            const p = preview()!;
            return (
              <div data-testid="gw-preview" style={{ display: "grid", gap: "8px" }}>
                <h3>Vorschau</h3>
                <p>
                  {p.summary.agent_count} Agents, {p.summary.room_count} Raeume.
                </p>
                <p class="muted">
                  Schicht-Verteilung:{" "}
                  {Object.entries(p.summary.shift_distribution)
                    .map(([k, v]) => `Set ${k}: ${v}`)
                    .join(", ")}
                </p>
                <label class="toggle-row" style={{ color: "var(--warn)" }}>
                  <input
                    type="checkbox"
                    data-testid="gw-overwrite"
                    checked={confirmOverwrite()}
                    onChange={(e) => setConfirmOverwrite(e.currentTarget.checked)}
                  />
                  Ueberschreibt eine evtl. laufende Firma (mode: fresh)
                </label>
                <div style={{ display: "flex", gap: "8px" }}>
                  <button disabled={busy()} onClick={() => setStep(4)}>
                    Zurueck
                  </button>
                  <button
                    class="primary"
                    data-testid="gw-deploy"
                    disabled={busy() || !confirmOverwrite()}
                    onClick={() => void runDeploy()}
                  >
                    Deploy
                  </button>
                </div>
              </div>
            );
          }}
        </Show>

        <Show when={step() === 6}>
          <div data-testid="gw-success" style={{ display: "grid", gap: "8px" }}>
            <h3>Fertig ✓</h3>
            <p class="muted">Die Firma wurde generiert und an den Daemon (Config-Apply #425) uebergeben.</p>
            <button
              onClick={() => {
                setStep(1);
                setPreview(null);
                setConfirmOverwrite(false);
              }}
            >
              Neue Firma
            </button>
          </div>
        </Show>

        <Show when={step() <= 4}>
          <div style={{ display: "flex", gap: "8px", "margin-top": "8px" }}>
            <button disabled={step() === 1 || busy()} onClick={back}>
              Zurueck
            </button>
            <button class="primary" data-testid="gw-next" disabled={!stepValid() || busy()} onClick={next}>
              {step() === 4 ? "Vorschau generieren" : "Weiter"}
            </button>
          </div>
        </Show>
      </div>
    </section>
  );
}
