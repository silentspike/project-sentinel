import { createMemo, createSignal, For, onCleanup, onMount, Show, type JSX } from "solid-js";
import { apiJson, type EbpfMetrics } from "../api";
import { ProgressBar, SearchFilter, LiveIndicator } from "../components/controls";
import { agentFilter, consoleStore, frameCount, setAgentFilter, status, type AgentRow } from "../stores/console";
import { roomDisplayName } from "../roomsMeta";
import { percentValue } from "./format";
import { setSelectedAgentId } from "../state/selection";
import { openPanel } from "../tiling/engine";

const STATUS_LABELS: Record<string, string> = {
  active: "Aktiv",
  suspended: "Pausiert",
  errored: "Fehler",
  despawned: "Despawned",
};

const BIO_FIELDS: { key: keyof AgentRow; label: string }[] = [
  { key: "hunger", label: "Hunger" },
  { key: "energy", label: "Energie" },
  { key: "stress", label: "Stress" },
  { key: "bladder", label: "Blase" },
  { key: "social_need", label: "Sozial" },
  { key: "caffeine_mg", label: "Koffein" },
];

function BioBar(props: { label: string; value: unknown }): JSX.Element {
  const pct = createMemo(() => percentValue(typeof props.value === "number" ? props.value : null));
  const level = createMemo(() => (pct() > 70 ? "high" : pct() > 40 ? "mid" : "low"));
  return (
    <div class="bio-bar">
      <span class="bio-bar__label">{props.label}</span>
      <span class="bio-bar__track" aria-hidden="true">
        <span class={`bio-bar__fill bio-bar__fill--${level()}`} style={{ width: `${pct()}%` }} />
      </span>
      <span class="bio-bar__value">{pct()}%</span>
    </div>
  );
}

export function AgentsView(): JSX.Element {
  const [ebpf, setEbpf] = createSignal<EbpfMetrics | null>(null);

  const loadEbpf = async () => {
    try {
      setEbpf(await apiJson<EbpfMetrics>("/api/metrics/ebpf"));
    } catch {
      setEbpf({ available: false, mode: "unavailable", stalled_count: 0, stalled_agents: [], prometheus: "offline" });
    }
  };

  onMount(() => {
    void loadEbpf();
    const timer = window.setInterval(() => void loadEbpf(), 10_000);
    onCleanup(() => window.clearInterval(timer));
  });

  const stalledNames = createMemo(() => new Set((ebpf()?.stalled_agents ?? []).map((agent) => agent.agent)));
  const filteredAgents = createMemo(() => {
    const q = agentFilter().toLowerCase().trim();
    return q
      ? consoleStore.agents.filter((agent) =>
          `${agent.name} ${agent.role} ${agent.current_room ?? ""}`.toLowerCase().includes(q),
        )
      : consoleStore.agents;
  });

  return (
    <section class="col view-panel" data-testid="view-agents">
      <div class="col__head view-head">
        <span>Agents</span>
        <LiveIndicator status={status()} />
      </div>
      <div class="col__body view-body">
        <div class="view-toolbar">
          <SearchFilter placeholder="Agents filtern..." onFilter={setAgentFilter} />
          <span class="pill" data-testid="agent-count">{filteredAgents().length} / {consoleStore.agents.length}</span>
          <span class="pill" data-testid="agents-frame-count">Frames {frameCount()}</span>
          <span class={`pill ${ebpf()?.available ? "pill-ok" : "pill-warn"}`}>
            eBPF {ebpf()?.available ? ebpf()?.mode : "N/A"}
          </span>
        </div>

        <ProgressBar label="Schicht-Auslastung" done={consoleStore.agents.length} total={Math.max(26, consoleStore.agents.length)} />

        <Show when={filteredAgents().length > 0} fallback={<p class="muted">Warte auf agent_live-Push.</p>}>
          <div class="agents-grid">
            <For each={filteredAgents()}>
              {(agent) => {
                const isStalled = createMemo(() => Boolean(agent.stalled) || stalledNames().has(agent.name));
                const statusKey = () => agent.status ?? "active";
                return (
                  <article
                    class={`agent-card ${isStalled() ? "agent-card--stalled" : ""}`}
                    data-testid="agent-card"
                    data-agent-id={agent.agent_id}
                    title="Deep View oeffnen"
                    style={{ cursor: "pointer" }}
                    onClick={() => {
                      // #428: open the Agent Deep View for this agent (shared selection signal).
                      setSelectedAgentId(agent.agent_id);
                      openPanel("agent-deep");
                    }}
                  >
                    <div class="agent-card__top">
                      <div>
                        <h3>{agent.name}</h3>
                        <p class="muted">{agent.role}</p>
                      </div>
                      <span class={`status-badge status-badge--${statusKey()}`}>{STATUS_LABELS[statusKey()] ?? statusKey()}</span>
                    </div>

                    <div class={`agent-room ${agent.in_transit ? "agent-room--transit" : ""}`}>
                      <Show
                        when={agent.in_transit}
                        fallback={roomDisplayName(agent.current_room)}
                      >
                        Unterwegs → {roomDisplayName(agent.transit_target)}
                      </Show>
                    </div>

                    <Show when={isStalled()}>
                      <div class="stall-indicator">Stalled</div>
                    </Show>

                    <div class="last-action">
                      <span>{agent.last_action || "Keine Aktion"}</span>
                      <Show when={agent.last_action_tick != null}>
                        <span class="last-action__tick">T{agent.last_action_tick}</span>
                      </Show>
                    </div>

                    <div class="agent-meta">
                      AGENT-{String(agent.agent_id).padStart(2, "0")}
                      <Show when={agent.shift_set != null}> | Schicht {agent.shift_set}</Show>
                    </div>

                    <div class="agent-mood">Stimmung: {agent.mood || "—"}</div>

                    <div class="bio-section">
                      <For each={BIO_FIELDS}>
                        {(field) => <BioBar label={field.label} value={agent[field.key]} />}
                      </For>
                    </div>
                  </article>
                );
              }}
            </For>
          </div>
        </Show>
      </div>
    </section>
  );
}
