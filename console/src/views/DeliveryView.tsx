import { createSignal, For, onCleanup, onMount, Show, type JSX } from "solid-js";
import { fetchPublicDeliveryLineage } from "./delivery/api";
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
  const [loading, setLoading] = createSignal(props.snapshot === undefined);
  const snapshot = () => {
    const value = props.snapshot ?? loaded();
    return value?.adapterReady ? value : undefined;
  };
  const failures = () => (snapshot() ? validateLineage(snapshot()!) : []);
  const controller = new AbortController();

  onMount(() => {
    if (props.snapshot) return;
    void (props.load ?? fetchPublicDeliveryLineage)(controller.signal)
      .then((value) => setLoaded(value))
      .catch(() => setLoaded(undefined))
      .finally(() => setLoading(false));
  });
  onCleanup(() => controller.abort());

  return (
    <div
      data-testid="view-delivery"
      class="col delivery-view"
      style={{ gap: "12px", padding: "12px", overflow: "auto", height: "100%" }}
    >
      <header class="control-card">
        <div style={{ display: "flex", "justify-content": "space-between", gap: "12px" }}>
          <div>
            <h3 style={{ margin: 0 }}>Delivery lineage</h3>
            <p class="muted" style={{ margin: "4px 0 0", "font-size": "12px" }}>
              Digest-bound candidate, QA, release and customer authority readback.
            </p>
          </div>
          <span data-testid="delivery-adapter-state" class="muted">
            {snapshot()?.adapterReady
              ? "Adapter ready"
              : loading()
                ? "Loading"
                : "Integration gated"}
          </span>
        </div>
      </header>

      <Show
        when={snapshot()}
        fallback={
          <section data-testid="delivery-unavailable" class="control-card">
            Delivery lineage is unavailable until the authenticated workflow adapter is ready.
          </section>
        }
      >
        {(safe) => (
          <>
            <Show when={failures().length > 0}>
              <section data-testid="delivery-invalid" class="control-card">
                Lineage rejected: {failures().join("; ")}
              </section>
            </Show>

            <Show when={failures().length === 0}>
              <section class="control-card">
                <div class="delivery-summary">
                  <span data-testid="delivery-project">{safe().projectLabel}</span>
                  <span>Revision {safe().revision}</span>
                  <span>{safe().nodes.length} lineage records</span>
                </div>
              </section>

              <section data-testid="delivery-lineage" class="control-card">
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

              <section class="control-card">
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
