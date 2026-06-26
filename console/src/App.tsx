import { createSignal, createEffect, onMount, For, Show, type JSX } from "solid-js";
import "./styles/tokens.css";
import { connectTransport } from "./stores/console";
import { authStatus, login as doLogin } from "./auth";
import {
  ToastContainer,
} from "./components/controls";
import { Tiling } from "./tiling/TilingLayout";
import { tilingTree, splitLeaf, closeLeaf, openPanel, type PanelKind } from "./tiling/engine";
import { AgentsView } from "./views/AgentsView";
import { ActivityView } from "./views/ActivityView";
import { ChaosView } from "./views/ChaosView";
import { ChatView } from "./views/ChatView";
import { CockpitView } from "./views/CockpitView";
import { ControlView } from "./views/ControlView";
import { FloorplanView } from "./views/FloorplanView";
import { MetricsView } from "./views/MetricsView";
import { ProfilingView } from "./views/ProfilingView";
import { TimeTravelView } from "./views/TimeTravelView";
import { GaiaWizardView } from "./views/GaiaWizardView";
import { AgentEditorView } from "./views/AgentEditorView";
import { ConfigEditorView } from "./views/ConfigEditorView";
import { SynthesisView } from "./views/SynthesisView";

// Mobile-Breakpoint via matchMedia (Desktop=Tiling, Mobile=BottomTabBar).
function useIsMobile() {
  const mq = typeof window !== "undefined" ? window.matchMedia("(max-width: 767px)") : null;
  const [m, setM] = createSignal(mq?.matches ?? false);
  mq?.addEventListener("change", (e) => setM(e.matches));
  return m;
}

const MOBILE_PANELS: PanelKind[] = ["agents", "floorplan", "metrics", "profiling", "cockpit", "activity", "chaos", "chat", "control", "timetravel", "gaia-wizard", "agent-editor", "config-editor", "synthesis"];
const PANEL_LABEL: Record<PanelKind, string> = {
  agents: "Agents",
  floorplan: "Floorplan",
  metrics: "Metrics",
  profiling: "Profiling",
  cockpit: "Cockpit",
  activity: "Activity",
  chaos: "Chaos",
  chat: "Chat",
  control: "Control",
  timetravel: "Zeitreise",
  "gaia-wizard": "Gaia Wizard",
  "agent-editor": "Agent Editor",
  "config-editor": "Config Editor",
  synthesis: "Synthesis",
};

function Login(props: { onOk: () => void }): JSX.Element {
  const [key, setKey] = createSignal("");
  // #474: distinguish wrong key ("invalid") from rate-limit ("rate-limited") for the operator.
  const [err, setErr] = createSignal<"" | "invalid" | "rate-limited">("");
  const submit = async () => {
    const res = await doLogin(key());
    if (res === "ok") props.onOk();
    else setErr(res);
  };
  return (
    <div data-testid="login" style={{ display: "grid", "place-items": "center", height: "100%" }}>
      <div class="col" style={{ padding: "28px", width: "min(92vw, 360px)" }}>
        <h1 style={{ "font-size": "1.3rem", margin: "0 0 4px" }}>Sentinel Gaia-Konsole</h1>
        <p class="muted" style={{ "margin-top": 0 }}>Operator-Login</p>
        <input
          data-testid="login-key" type="password" placeholder="Operator-Key" style={{ width: "100%", "margin-bottom": "10px" }}
          value={key()} onInput={(e) => setKey(e.currentTarget.value)}
          onKeyDown={(e) => e.key === "Enter" && void submit()}
        />
        <button class="primary" data-testid="login-submit" style={{ width: "100%" }} onClick={() => void submit()}>Anmelden</button>
        <Show when={err()}>
          <p data-testid="login-error" style={{ color: "var(--danger)", "font-size": "13px" }}>
            {err() === "rate-limited"
              ? "Zu viele Fehlversuche — bitte kurz warten."
              : "Ungueltiger Key."}
          </p>
        </Show>
      </div>
    </div>
  );
}

const PANELS: Record<PanelKind, () => JSX.Element> = {
  agents: AgentsView,
  floorplan: FloorplanView,
  metrics: MetricsView,
  profiling: ProfilingView,
  cockpit: CockpitView,
  activity: ActivityView,
  chaos: ChaosView,
  chat: ChatView,
  control: ControlView,
  timetravel: TimeTravelView,
  "gaia-wizard": GaiaWizardView,
  "agent-editor": AgentEditorView,
  "config-editor": ConfigEditorView,
  synthesis: SynthesisView,
};

// Tile-Chrome: kompakte Leiste (Split horizontal/vertikal, Schliessen) ueber dem Panel.
function renderPanel(panel: PanelKind, leafId: string): JSX.Element {
  return (
    <div style={{ height: "100%", display: "flex", "flex-direction": "column", "min-height": 0 }}>
      <div style={{ display: "flex", gap: "4px", padding: "3px 6px", background: "var(--surface-1)", "border-bottom": "1px solid var(--border)" }}>
        <span class="muted" style={{ flex: 1, "font-size": "11px", "align-self": "center" }}>{PANEL_LABEL[panel]}</span>
        <button data-testid={`split-row-${panel}`} title="rechts splitten" style={{ padding: "1px 7px" }} onClick={() => splitLeaf(leafId, "row", "floorplan")}>⬌</button>
        <button data-testid={`split-col-${panel}`} title="unten splitten" style={{ padding: "1px 7px" }} onClick={() => splitLeaf(leafId, "col", "floorplan")}>⬍</button>
        <button data-testid={`close-${panel}`} title="schliessen" style={{ padding: "1px 7px" }} onClick={() => closeLeaf(leafId)}>✕</button>
      </div>
      <div style={{ flex: 1, "min-height": 0, overflow: "auto" }}>{PANELS[panel]()}</div>
    </div>
  );
}

export default function App(): JSX.Element {
  const [authed, setAuthed] = createSignal(false);
  const isMobile = useIsMobile();
  const [tab, setTab] = createSignal<PanelKind>("agents");

  onMount(async () => {
    setAuthed(await authStatus());
  });
  // WT erst nach Auth verbinden — der Connect holt ein Ticket von /api/wt-ticket (require_auth);
  // URL = same-origin (window.location.origin). Browser senden bei WT keine Cookies -> Ticket-Auth.
  createEffect(() => {
    if (authed()) connectTransport(window.location.origin);
  });

  return (
    <Show when={authed()} fallback={<Login onOk={() => setAuthed(true)} />}>
      <div data-testid="shell" style={{ height: "100%", display: "flex", "flex-direction": "column" }}>
        <Show
          when={!isMobile()}
          fallback={
            <>
              <main style={{ flex: 1, overflow: "auto", padding: "var(--gap)" }}>
                {PANELS[tab()]()}
              </main>
              <nav data-testid="bottom-tabbar" style={{ display: "flex", overflow: "auto", "border-top": "1px solid var(--border)", background: "var(--surface-1)" }}>
                <For each={MOBILE_PANELS}>
                  {(p) => (
                    <button data-testid={`tab-${p}`} onClick={() => setTab(p)}
                      style={{ flex: "1 0 auto", "border-radius": 0, border: "none", background: tab() === p ? "var(--surface-2)" : "transparent" }}>
                      {PANEL_LABEL[p]}
                    </button>
                  )}
                </For>
              </nav>
            </>
          }
        >
          <div data-testid="tiling-toolbar" style={{ display: "flex", gap: "8px", padding: "6px var(--gap)", "border-bottom": "1px solid var(--border)", background: "var(--surface-0)" }}>
            <span class="muted" style={{ "align-self": "center", "font-size": "12px" }}>Workspace (niri layout)</span>
            <For each={MOBILE_PANELS}>
              {(panel) => <button data-testid={`open-${panel}`} onClick={() => openPanel(panel)}>{PANEL_LABEL[panel]}</button>}
            </For>
          </div>
          <main data-testid="tiling-root" style={{ flex: 1, "min-height": 0, padding: "var(--gap)" }}>
            <Tiling node={tilingTree.root} renderPanel={renderPanel} />
          </main>
        </Show>
      </div>
      <ToastContainer />
    </Show>
  );
}
