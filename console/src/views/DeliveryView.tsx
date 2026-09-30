import { createSignal, createUniqueId, For, onCleanup, onMount, Show, type JSX } from "solid-js";
import { DeliveryLineageUnavailable, fetchPublicDeliveryLineage, initialDeliveryScope, validDeliveryScope } from "./delivery/api";
import {
  formatMinorUnits,
  shortDigest,
  validateLineage,
  type PublicDeliveryLineageDto,
} from "./delivery/lineage";

export interface DeliveryViewProps {
  snapshot?: PublicDeliveryLineageDto;
  load?: (signal: AbortSignal) => Promise<PublicDeliveryLineageDto>;
}

const STAGE_LABELS: Record<string, string> = {
  customer_request: "Customer request",
  agreement: "Agreement",
  project: "Project",
  work_item: "Work item",
  participant: "Participant",
  decision: "Decision",
  handoff: "Handoff",
  blocker: "Blocker",
  candidate: "Candidate",
  qa: "Independent QA",
  workbench: "Workbench",
  artifact: "Artifact",
  review: "Review",
  test: "Test",
  finding: "Finding",
  approval: "Approval",
  manifest: "Manifest",
  release: "Release",
  delivery: "Customer delivery",
  acceptance: "Acceptance",
  closeout: "Closeout",
  rollback: "Rollback",
};

export function DeliveryView(props: DeliveryViewProps): JSX.Element {
  const [loaded, setLoaded] = createSignal<PublicDeliveryLineageDto | undefined>(props.snapshot);
  const [scope, setScope] = createSignal(initialDeliveryScope());
  const [loading, setLoading] = createSignal(false);
  const [readError, setReadError] = createSignal<DeliveryLineageUnavailable["kind"]>();
  const scopeId = createUniqueId();
  const snapshot = () => {
    const value = props.snapshot ?? loaded();
    return value?.adapterReady ? value : undefined;
  };
  const failures = () => (snapshot() ? validateLineage(snapshot()!) : []);
  let controller: AbortController | undefined;
  let generation = 0;
  const cancel = () => {
    generation += 1;
    controller?.abort();
  };
  const editScope = (field: "tenantId" | "projectId", value: string) => {
    cancel();
    setScope((current) => ({ ...current, [field]: value }));
    setLoaded(undefined);
    setLoading(false);
    setReadError(undefined);
  };
  const load = async (legacyLoader = false) => {
    cancel();
    setLoaded(undefined);
    setReadError(undefined);
    setLoading(false);
    const selected = { ...scope() };
    if (!validDeliveryScope(selected) && !(legacyLoader && props.load)) return;
    const current = generation;
    const request = new AbortController();
    controller = request;
    setLoading(true);
    try {
      const value = await (props.load
        ? props.load(request.signal)
        : fetchPublicDeliveryLineage(request.signal, selected));
      if (current === generation && !request.signal.aborted) setLoaded(value);
    } catch (error) {
      if (current === generation && !request.signal.aborted)
        setReadError(error instanceof DeliveryLineageUnavailable ? error.kind : "unavailable");
    } finally {
      if (current === generation && !request.signal.aborted) setLoading(false);
    }
  };
  const status = () => loading() ? "Loading"
    : readError() === "inaccessible" ? "No accessible delivery lineage"
    : readError() === "access" ? "Access unavailable"
    : readError() ? "Read error"
    : (props.snapshot ?? loaded()) ? (props.snapshot ?? loaded())!.adapterReady ? "Adapter ready" : "Integration gated"
    : !scope().tenantId || !scope().projectId ? "Select project"
    : validDeliveryScope(scope()) ? "Ready to load" : "Invalid scope";

  onMount(() => {
    if (props.snapshot) return;
    if (props.load || validDeliveryScope(scope())) void load(true);
  });
  onCleanup(cancel);

  return (
    <div
      data-testid="view-delivery"
      class="col delivery-view"
      style={{ gap: "12px", padding: "12px", overflow: "auto", height: "100%" }}
    >
      <header>
        <div style={{ display: "flex", "justify-content": "space-between", "flex-wrap": "wrap", gap: "12px" }}>
          <div>
            <h3 style={{ margin: 0 }}>Delivery lineage</h3>
          </div>
          <span data-testid="delivery-adapter-state" class="muted">
            {status()}
          </span>
        </div>
      </header>

      <Show when={!props.snapshot}>
        <form onSubmit={(event) => { event.preventDefault(); void load(); }}
          style={{ display: "flex", "flex-wrap": "wrap", gap: "12px", "align-items": "end" }}>
          <div style={{ flex: "1 1 180px", "min-width": 0 }}>
            <label for={`${scopeId}-tenant`}>Tenant</label>
            <input id={`${scopeId}-tenant`} value={scope().tenantId} maxLength={128}
              style={{ width: "100%", "box-sizing": "border-box" }}
              onInput={(event) => editScope("tenantId", event.currentTarget.value)} />
          </div>
          <div style={{ flex: "1 1 180px", "min-width": 0 }}>
            <label for={`${scopeId}-project`}>Project</label>
            <input id={`${scopeId}-project`} value={scope().projectId} maxLength={128}
              style={{ width: "100%", "box-sizing": "border-box" }}
              onInput={(event) => editScope("projectId", event.currentTarget.value)} />
          </div>
          <button type="submit" disabled={!validDeliveryScope(scope())}>Load</button>
        </form>
      </Show>

      <Show
        when={snapshot()}
        fallback={
          <section data-testid={loading() ? "delivery-loading" : readError() ? "delivery-read-error"
            : loaded() ? "delivery-unavailable" : "delivery-select-project"} role="status">
            {status()}
          </section>
        }
      >
        {(safe) => (
          <>
            <Show when={failures().length > 0}>
              <section data-testid="delivery-invalid">
                Lineage rejected: {failures().join("; ")}
              </section>
            </Show>

            <Show when={failures().length === 0}>
              <section>
                <div class="delivery-summary">
                  <span data-testid="delivery-project">{safe().projectLabel}</span>
                  <span>Revision {safe().revision}</span>
                  <span>{safe().nodes.length} lineage records</span>
                </div>
              </section>

              <section data-testid="delivery-lineage">
                <For each={safe().nodes}>
                  {(node, index) => (
                    <article
                      data-testid="delivery-lineage-node"
                      class="delivery-lineage-node"
                    >
                      <strong class="delivery-node-stage">
                        {index() + 1}. {STAGE_LABELS[node.stage]}
                      </strong>
                      <span class="delivery-node-label">
                        {node.label} <span class="muted">({node.state})</span>
                      </span>
                      <span data-testid="delivery-authority">{node.actorRole}</span>
                      <code data-testid="delivery-digest">
                        g{node.generation} {shortDigest(node.digest)}
                      </code>
                      <span data-testid="delivery-cost">
                        {node.costMinor === undefined || node.currency === undefined
                          ? "Cost n/a"
                          : formatMinorUnits(node.costMinor, node.currency)}
                      </span>
                    </article>
                  )}
                </For>
              </section>

              <section>
                <h4 style={{ margin: "0 0 8px" }}>Blockers</h4>
                <Show
                  when={safe().blockers.length > 0}
                  fallback={<span data-testid="delivery-no-blockers">No active blockers</span>}
                >
                  <ul data-testid="delivery-blockers">
                    <For each={safe().blockers}>{(blocker) => <li>{blocker}</li>}</For>
                  </ul>
                </Show>
              </section>
            </Show>
          </>
        )}
      </Show>
    </div>
  );
}
