import { createSignal, createMemo, onMount, For, Show, type JSX } from "solid-js";
import "./styles/tokens.css";
import { consoleStore, status, frameCount, connectTransport, ingestFrame, type AgentRow } from "./stores/console";
import { authStatus, login as doLogin } from "./auth";
import {
  ProgressBar, SearchFilter, StatusDropdown, LiveIndicator, ThemeToggle,
  ToastContainer, addToast, type Status,
} from "./components/controls";
import { VirtualScroller } from "./components/VirtualScroller";
import { Tiling } from "./tiling/TilingLayout";
import { tilingTree, splitLeaf, closeLeaf, openPanel, type PanelKind } from "./tiling/engine";

// Mobile-Breakpoint via matchMedia (Desktop=Tiling, Mobile=BottomTabBar).
function useIsMobile() {
  const mq = typeof window !== "undefined" ? window.matchMedia("(max-width: 767px)") : null;
  const [m, setM] = createSignal(mq?.matches ?? false);
  mq?.addEventListener("change", (e) => setM(e.matches));
  return m;
}

const PILLARS = ["dashboard", "control", "chat"] as const;
type Pillar = (typeof PILLARS)[number];
const PILLAR_LABEL: Record<Pillar, string> = { dashboard: "Dashboard", control: "Control-Center", chat: "Chat" };

function Login(props: { onOk: () => void }): JSX.Element {
  const [key, setKey] = createSignal("");
  const [err, setErr] = createSignal(false);
  const submit = async () => {
    if (await doLogin(key())) props.onOk();
    else setErr(true);
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
        <Show when={err()}><p style={{ color: "var(--danger)", "font-size": "13px" }}>Ungueltiger Key.</p></Show>
      </div>
    </div>
  );
}

function DashboardCol(): JSX.Element {
  const bigList = createMemo(() => Array.from({ length: 10000 }, (_, i) => ({ id: i, label: `Zeile ${i}` })));
  const simulatePush = () =>
    ingestFrame("agent_live", {
      agents: [
        { agent_id: 1, name: "Thomas Mueller", role: "CEO", current_room: "buero-ceo", energy: 0.8, stress: 0.2, mood: "fokussiert" },
        { agent_id: 5, name: "Andreas Wolff", role: "Lead", current_room: "buero-dev-1", energy: 0.6, stress: 0.4, mood: "konzentriert" },
      ] satisfies AgentRow[],
    });
  return (
    <section class="col" style={{ height: "100%" }} data-testid="col-dashboard">
      <div class="col__head">Dashboard <LiveIndicator status={status()} /></div>
      <div class="col__body">
        <p class="muted">Frames: <span data-testid="frame-count" class="pill">{frameCount()}</span> · Topic: <span data-testid="last-topic" class="pill">{consoleStore.lastTopic ?? "—"}</span></p>
        <ProgressBar label="Schicht-Auslastung" done={Math.min(consoleStore.agents.length, 26)} total={26} />
        <button data-testid="simulate-push" onClick={simulatePush}>Push simulieren</button>
        <h3 style={{ "margin-bottom": "4px" }}>Agents (reaktiv)</h3>
        <Show when={consoleStore.agents.length > 0} fallback={<p class="muted">noch keine Agents</p>}>
          <For each={consoleStore.agents}>
            {(a) => (
              <div data-testid="agent-row" style={{ display: "flex", "justify-content": "space-between", padding: "4px 0", "border-bottom": "1px solid var(--border)" }}>
                <span>{a.name} <span class="muted">· {a.role}</span></span><span class="pill">{a.current_room ?? "—"}</span>
              </div>
            )}
          </For>
        </Show>
        <h3 style={{ "margin-bottom": "4px" }}>VirtualScroller (N=10000)</h3>
        <VirtualScroller items={bigList()} rowHeight={28} height={180} renderRow={(it) => <span data-testid="vrow">{it.label}</span>} />
      </div>
    </section>
  );
}

function ControlCol(): JSX.Element {
  const [st, setSt] = createSignal<Status>("pending");
  const [filter, setFilter] = createSignal("");
  return (
    <section class="col" style={{ height: "100%" }} data-testid="col-control">
      <div class="col__head">Control-Center</div>
      <div class="col__body" style={{ display: "grid", gap: "10px" }}>
        <SearchFilter placeholder="Agents filtern…" onFilter={setFilter} />
        <p class="muted" style={{ "font-size": "12px" }}>Filter: <span data-testid="filter-value">{filter() || "—"}</span></p>
        <div style={{ display: "flex", gap: "8px", "align-items": "center" }}><span>Status:</span><StatusDropdown value={st()} onChange={setSt} /></div>
        <div style={{ display: "flex", gap: "8px" }}><ThemeToggle /><button data-testid="toast-btn" onClick={() => addToast("Aktion ausgefuehrt", "ok")}>Toast</button></div>
      </div>
    </section>
  );
}

function ChatCol(): JSX.Element {
  return (
    <section class="col" style={{ height: "100%" }} data-testid="col-chat">
      <div class="col__head">Chat</div>
      <div class="col__body"><p class="muted">Room-Chat · 1:1-Agent-DM · Invite (Phase 4).</p></div>
    </section>
  );
}

function FloorplanCol(): JSX.Element {
  return (
    <section class="col" style={{ height: "100%" }} data-testid="col-floorplan">
      <div class="col__head">Floorplan</div>
      <div class="col__body">
        <p class="muted">2D-Floorplan (WebGL/Canvas, eigenes View-Issue). Von Gaia kontextuell geoeffnet.</p>
        <div style={{ height: "120px", border: "1px dashed var(--border)", "border-radius": "6px", display: "grid", "place-items": "center" }} class="muted">[ Floorplan-Canvas ]</div>
      </div>
    </section>
  );
}

const PANELS: Record<PanelKind, () => JSX.Element> = {
  dashboard: DashboardCol, control: ControlCol, chat: ChatCol, floorplan: FloorplanCol,
};

// Tile-Chrome: kompakte Leiste (Split horizontal/vertikal, Schliessen) ueber dem Panel.
function renderPanel(panel: PanelKind, leafId: string): JSX.Element {
  return (
    <div style={{ height: "100%", display: "flex", "flex-direction": "column", "min-height": 0 }}>
      <div style={{ display: "flex", gap: "4px", padding: "3px 6px", background: "var(--surface-1)", "border-bottom": "1px solid var(--border)" }}>
        <span class="muted" style={{ flex: 1, "font-size": "11px", "align-self": "center" }}>{panel}</span>
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
  const [tab, setTab] = createSignal<Pillar>("dashboard");

  onMount(async () => {
    setAuthed(await authStatus());
    const host = window.location.hostname || "127.0.0.1";
    connectTransport(`https://${host}:4434`);
  });

  return (
    <Show when={authed()} fallback={<Login onOk={() => setAuthed(true)} />}>
      <div data-testid="shell" style={{ height: "100%", display: "flex", "flex-direction": "column" }}>
        <Show
          when={!isMobile()}
          fallback={
            <>
              <main style={{ flex: 1, overflow: "auto", padding: "var(--gap)" }}>
                {tab() === "dashboard" ? <DashboardCol /> : tab() === "control" ? <ControlCol /> : <ChatCol />}
              </main>
              <nav data-testid="bottom-tabbar" style={{ display: "flex", "border-top": "1px solid var(--border)", background: "var(--surface-1)" }}>
                <For each={PILLARS}>
                  {(p) => (
                    <button data-testid={`tab-${p}`} onClick={() => setTab(p)}
                      style={{ flex: 1, "border-radius": 0, border: "none", background: tab() === p ? "var(--surface-2)" : "transparent" }}>
                      {PILLAR_LABEL[p]}
                    </button>
                  )}
                </For>
              </nav>
            </>
          }
        >
          <div data-testid="tiling-toolbar" style={{ display: "flex", gap: "8px", padding: "6px var(--gap)", "border-bottom": "1px solid var(--border)", background: "var(--surface-0)" }}>
            <span class="muted" style={{ "align-self": "center", "font-size": "12px" }}>Workspace (niri-Stil)</span>
            <button data-testid="gaia-open-floorplan" onClick={() => openPanel("floorplan")}>Gaia: zeig Floorplan</button>
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
