import { For, Show, type JSX } from "solid-js";
import {
  shortDigest,
  validateLineage,
  type PublicDeliveryLineageDto,
} from "./delivery/lineage";

export interface DeliveryViewProps {
  snapshot?: PublicDeliveryLineageDto;
}

const STAGE_LABELS: Record<string, string> = {
  candidate: "Candidate",
  qa: "Independent QA",
  release: "Release",
  delivery: "Customer delivery",
  acceptance: "Acceptance",
  closeout: "Closeout",
  rollback: "Rollback",
};

export function DeliveryView(props: DeliveryViewProps): JSX.Element {
  const snapshot = () => (props.snapshot?.adapterReady ? props.snapshot : undefined);
  const failures = () => (snapshot() ? validateLineage(snapshot()!) : []);

  return (
    <div
      data-testid="view-delivery"
      class="col"
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
            {snapshot()?.adapterReady ? "Adapter ready" : "Integration gated"}
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
                <div
                  style={{
                    display: "grid",
                    "grid-template-columns": "repeat(3, minmax(0, 1fr))",
                    gap: "8px",
                  }}
                >
                  <span data-testid="delivery-project">Project {safe().projectLabel}</span>
                  <span>Revision {safe().revision}</span>
                  <span>{safe().nodes.length} lineage records</span>
                </div>
              </section>

              <section data-testid="delivery-lineage" class="control-card">
                <For each={safe().nodes}>
                  {(node, index) => (
                    <article
                      data-testid="delivery-lineage-node"
                      style={{
                        display: "grid",
                        "grid-template-columns": "140px 1fr 150px 180px 100px",
                        gap: "10px",
                        padding: "9px 0",
                        "border-bottom": "1px solid var(--border, #333)",
                      }}
                    >
                      <strong>
                        {index() + 1}. {STAGE_LABELS[node.stage]}
                      </strong>
                      <span>
                        {node.label} <span class="muted">({node.state})</span>
                      </span>
                      <span data-testid="delivery-authority">{node.actorRole}</span>
                      <code data-testid="delivery-digest">
                        g{node.generation} {shortDigest(node.digest)}
                      </code>
                      <span data-testid="delivery-cost">
                        {node.costUsd === undefined ? "Cost n/a" : `$${node.costUsd.toFixed(2)}`}
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
