import { createSignal, createMemo, onMount, For, Show, type JSX } from "solid-js";
import "./styles/tokens.css";
import { consoleStore, status, frameCount, connectTransport, ingestFrame, type AgentRow } from "./stores/console";
import { authStatus, login as doLogin } from "./auth";
import {
  ProgressBar, SearchFilter, StatusDropdown, LiveIndicator, ThemeToggle,
  ToastContainer, addToast, type Status,
} from "./components/controls";
import { VirtualScroller } from "./components/VirtualScroller";

// Mobile-Breakpoint via matchMedia (Desktop=3 Saeulen, Mobile=BottomTabBar).
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
  // Demo eines gepushten Frames (gleicher reconcile-Pfad wie ein echter WebTransport-Push).
  const simulatePush = () =>
    ingestFrame("agent_live", {
      agents: [
        { agent_id: 1, name: "Thomas Mueller", role: "CEO", current_room: "buero-ceo", energy: 0.8, stress: 0.2, mood: "fokussiert" },
        { agent_id: 5, name: "Andreas Wolff", role: "Lead", current_room: "buero-dev-1", energy: 0.6, stress: 0.4, mood: "konzentriert" },
      ] satisfies AgentRow[],
    });
  return (
    <section class="col" data-testid="col-dashboard">
      <div class="col__head">Dashboard <LiveIndicator status={status()} /></div>
      <div class="col__body">
        <p class="muted">Frames empfangen: <span data-testid="frame-count" class="pill">{frameCount()}</span> · letztes Topic: <span data-testid="last-topic" class="pill">{consoleStore.lastTopic ?? "—"}</span></p>
        <ProgressBar label="Schicht-Auslastung" done={Math.min(consoleStore.agents.length, 26)} total={26} />
        <button data-testid="simulate-push" onClick={simulatePush}>Push simulieren</button>
        <h3 style={{ "margin-bottom": "4px" }}>Agents (reaktiv)</h3>
        <div data-testid="agent-list">
          <Show when={consoleStore.agents.length > 0} fallback={<p class="muted">noch keine Agents (Push simulieren / Backend verbinden)</p>}>
            <For each={consoleStore.agents}>
              {(a) => (
                <div data-testid="agent-row" style={{ display: "flex", "justify-content": "space-between", padding: "4px 0", "border-bottom": "1px solid var(--border)" }}>
                  <span>{a.name} <span class="muted">· {a.role}</span></span>
                  <span class="pill">{a.current_room ?? "—"}</span>
                </div>
              )}
            </For>
          </Show>
        </div>
        <h3 style={{ "margin-bottom": "4px" }}>VirtualScroller (N=10000)</h3>
        <VirtualScroller items={bigList()} rowHeight={28} height={220}
          renderRow={(it) => <span data-testid="vrow">{it.label}</span>} />
      </div>
    </section>
  );
}

function ControlCol(): JSX.Element {
  const [st, setSt] = createSignal<Status>("pending");
  const [filter, setFilter] = createSignal("");
  return (
    <section class="col" data-testid="col-control">
      <div class="col__head">Control-Center</div>
      <div class="col__body" style={{ display: "grid", gap: "10px" }}>
        <SearchFilter placeholder="Agents filtern…" onFilter={setFilter} />
        <p class="muted" style={{ "font-size": "12px" }}>Filter: <span data-testid="filter-value">{filter() || "—"}</span></p>
        <div style={{ display: "flex", gap: "8px", "align-items": "center" }}>
          <span>Status:</span><StatusDropdown value={st()} onChange={setSt} />
        </div>
        <div style={{ display: "flex", gap: "8px" }}>
          <ThemeToggle />
          <button data-testid="toast-btn" onClick={() => addToast("Aktion ausgefuehrt", "ok")}>Toast</button>
        </div>
      </div>
    </section>
  );
}

function ChatCol(): JSX.Element {
  return (
    <section class="col" data-testid="col-chat">
      <div class="col__head">Chat</div>
      <div class="col__body"><p class="muted">Room-Chat · 1:1-Agent-DM · Invite (Phase 4). Hier rendern Konversationen.</p></div>
    </section>
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

  const cols: Record<Pillar, () => JSX.Element> = { dashboard: DashboardCol, control: ControlCol, chat: ChatCol };

  return (
    <Show when={authed()} fallback={<Login onOk={() => setAuthed(true)} />}>
      <div data-testid="shell" style={{ height: "100%", display: "flex", "flex-direction": "column" }}>
        <Show
          when={!isMobile()}
          fallback={
            <>
              <main style={{ flex: 1, overflow: "auto", padding: "var(--gap)" }}>{cols[tab()]()}</main>
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
          <main style={{ flex: 1, display: "grid", "grid-template-columns": "1fr 1fr 1fr", gap: "var(--gap)", padding: "var(--gap)", "min-height": 0 }}>
            <DashboardCol />
            <ControlCol />
            <ChatCol />
          </main>
        </Show>
      </div>
      <ToastContainer />
    </Show>
  );
}
