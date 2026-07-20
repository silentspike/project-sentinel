import { createSignal, For, Show, onMount, onCleanup, type JSX } from "solid-js";
import { apiJson, type AgentConfig } from "../api";
import { openPanel } from "../tiling/engine";
import { setSelectedAgentId } from "../state/selection";

// #424: read-only Org Chart — the company hierarchy (department -> role -> agent) built from the
// agent configs (GET /api/config/agents, #420), with the hierarchy tier per node. Read-first; clicking
// an agent jumps to the Agent Editor (#422), pre-selected via the shared selectedAgentId signal
// (reports_to/direct_reports are shown as node metadata; the primary tree axis is dept->role->agent).

interface RoleGroup {
  role: string;
  agents: AgentConfig[];
}
interface DeptGroup {
  department: string;
  roles: RoleGroup[];
  count: number;
}

/** Pure + testable: group agents department -> role -> agent, stably sorted. */
export function buildOrgTree(agents: AgentConfig[]): DeptGroup[] {
  const byDept = new Map<string, Map<string, AgentConfig[]>>();
  for (const a of agents) {
    const dept = a.identity.department || "—";
    const role = a.identity.role || "—";
    if (!byDept.has(dept)) byDept.set(dept, new Map());
    const roles = byDept.get(dept)!;
    if (!roles.has(role)) roles.set(role, []);
    roles.get(role)!.push(a);
  }
  return [...byDept.entries()]
    .sort(([a], [b]) => a.localeCompare(b))
    .map(([department, roles]) => {
      const roleGroups: RoleGroup[] = [...roles.entries()]
        .sort(([a], [b]) => a.localeCompare(b))
        .map(([role, list]) => ({
          role,
          agents: [...list].sort((x, y) => x.identity.name.localeCompare(y.identity.name)),
        }));
      const count = roleGroups.reduce((n, r) => n + r.agents.length, 0);
      return { department, roles: roleGroups, count };
    });
}

/** Organization hierarchy tier: legacy omission -> "—" (never guessed/mapped). */
function tierOf(a: AgentConfig): string {
  const t = a.identity.tier;
  return t === 1 || t === 2 || t === 3 ? String(t) : "—";
}

/** reports_to / direct_reports as a compact node-metadata hint. */
function reportingHint(a: AgentConfig): string {
  const parts: string[] = [];
  if (a.identity.reports_to) parts.push(`↑ ${a.identity.reports_to}`);
  const n = a.identity.direct_reports?.length ?? 0;
  if (n > 0) parts.push(`${n} report${n === 1 ? "" : "s"}`);
  return parts.length ? ` · ${parts.join(" · ")}` : "";
}

export function OrgChartView(): JSX.Element {
  const [agents, setAgents] = createSignal<AgentConfig[]>([]);
  const [loading, setLoading] = createSignal(true);
  const [loadError, setLoadError] = createSignal<string | null>(null);

  async function load(): Promise<void> {
    try {
      const v = await apiJson<AgentConfig[]>("/api/config/agents");
      if (Array.isArray(v)) setAgents(v);
      setLoadError(null);
    } catch (error) {
      setLoadError(error instanceof Error ? error.message : "Org Chart konnte nicht geladen werden");
    } finally {
      setLoading(false);
    }
  }

  onMount(() => {
    void load();
    const timer = window.setInterval(() => void load(), 10_000);
    onCleanup(() => window.clearInterval(timer));
  });

  function openAgent(a: AgentConfig): void {
    setSelectedAgentId(a.identity.id);
    openPanel("agent-editor");
  }

  return (
    <div
      data-testid="view-org-chart"
      class="col"
      style={{ gap: "10px", padding: "12px", overflow: "auto", height: "100%" }}
    >
      <h3 style={{ margin: "0 0 4px" }}>Org Chart — {agents().length} Agents</h3>
      <p class="muted" style={{ "margin-top": 0, "font-size": "12px" }}>
        Hierarchie aus den Agent-Configs (Abteilung → Rolle → Agent). Klick auf einen Agent öffnet den
        Agent Editor. Hierarchy tier is read from identity.tier; nano_runtime remains separate.
      </p>
      <Show
        when={!loading()}
        fallback={<p class="muted" data-testid="org-loading">Agent-Configs werden geladen.</p>}
      >
        <Show
          when={!loadError()}
          fallback={<p class="degraded-panel" data-testid="org-error" role="alert">{loadError()}</p>}
        >
          <Show
            when={agents().length > 0}
            fallback={<p class="muted" data-testid="org-empty">Keine Agent-Configs geladen.</p>}
          >
            <For each={buildOrgTree(agents())}>
          {(dept) => (
            <section data-testid="org-dept" class="control-card" style={{ padding: "8px" }}>
              <div style={{ "font-weight": "600", "margin-bottom": "4px" }}>
                {dept.department}{" "}
                <span class="muted" style={{ "font-weight": "400" }}>· {dept.count}</span>
              </div>
              <For each={dept.roles}>
                {(roleGroup) => (
                  <div data-testid="org-role" style={{ "margin-left": "12px", "margin-bottom": "4px" }}>
                    <div class="muted" style={{ "font-size": "12px", "font-family": "monospace" }}>
                      {roleGroup.role}
                    </div>
                    <For each={roleGroup.agents}>
                      {(a) => (
                        <button
                          type="button"
                          data-testid="org-agent-node"
                          onClick={() => openAgent(a)}
                          title="Im Agent-Editor öffnen"
                          aria-label={`${a.identity.name} im Agent-Editor öffnen, hierarchy tier ${tierOf(a)}`}
                          style={{
                            "margin-left": "12px",
                            display: "grid",
                            "grid-template-columns": "minmax(0, 1fr) auto",
                            gap: "8px",
                            cursor: "pointer",
                            padding: "2px 4px",
                            "border-radius": "4px",
                            "font-size": "13px",
                            width: "100%",
                            border: "0",
                            background: "transparent",
                            color: "inherit",
                            "text-align": "left",
                          }}
                        >
                          <span>
                            {a.identity.name}
                            <span class="muted" style={{ "font-size": "11px" }}>
                              {reportingHint(a)}
                            </span>
                          </span>
                          <span
                            data-testid="org-tier"
                            aria-label={`Organization hierarchy tier ${tierOf(a)}`}
                            class="muted"
                            style={{ "font-family": "monospace" }}
                          >
                            hierarchy tier: {tierOf(a)}
                          </span>
                        </button>
                      )}
                    </For>
                  </div>
                )}
              </For>
            </section>
          )}
            </For>
          </Show>
        </Show>
      </Show>
    </div>
  );
}
