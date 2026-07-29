import { describe, expect, it } from "vitest";
import { render } from "@solidjs/testing-library";
import { DeliveryView } from "../src/views/DeliveryView";
import {
  publicLineage,
  validateLineage,
  type DeliveryLineageSnapshot,
} from "../src/views/delivery/lineage";

const DIGEST = "a".repeat(64);

function snapshot(): DeliveryLineageSnapshot {
  return {
    schemaVersion: 1,
    tenantId: "tenant-private",
    projectId: "project-42",
    revision: 9,
    nodes: [
      {
        id: "candidate-1",
        stage: "candidate",
        label: "Static site candidate",
        state: "qa_assigned",
        digest: DIGEST,
        generation: 3,
        actorRole: "developer",
        costUsd: 1.25,
      },
      {
        id: "qa-1",
        stage: "qa",
        label: "Independent gate",
        state: "completed_pass",
        digest: "b".repeat(64),
        generation: 1,
        actorRole: "qa",
      },
      {
        id: "release-1",
        stage: "release",
        label: "Approved release",
        state: "active",
        digest: "c".repeat(64),
        generation: 1,
        actorRole: "release_manager",
      },
    ],
    edges: [
      { from: "candidate-1", to: "qa-1" },
      { from: "qa-1", to: "release-1" },
    ],
    blockers: [],
    adapterReady: true,
    readAt: "2026-07-29T00:00:00Z",
  };
}

describe("delivery lineage model", () => {
  it("validates digest-bound nodes and redacts tenant authority", () => {
    const value = snapshot();
    expect(validateLineage(value)).toEqual([]);
    expect(publicLineage(value).tenantId).toBe("redacted");

    value.edges.push({ from: "release-1", to: "missing" });
    expect(validateLineage(value)).toContain("dangling edge: release-1->missing");
  });
});

describe("DeliveryView", () => {
  it("fails closed while the productive adapter is not connected", () => {
    const { getByTestId } = render(() => DeliveryView({}));
    expect(getByTestId("delivery-unavailable")).toBeTruthy();
    expect(getByTestId("delivery-adapter-state").textContent).toContain("Integration gated");
  });

  it("does not render a supplied snapshot when its adapter is not ready", () => {
    const value = snapshot();
    value.adapterReady = false;
    const { getByTestId, queryByTestId } = render(() =>
      DeliveryView({ snapshot: value }),
    );
    expect(getByTestId("delivery-unavailable")).toBeTruthy();
    expect(queryByTestId("delivery-lineage")).toBeNull();
  });

  it("renders public-safe digest, authority and lineage readback", () => {
    const { getByTestId, getAllByTestId } = render(() =>
      DeliveryView({ snapshot: snapshot() }),
    );
    expect(getByTestId("delivery-project").textContent).toContain("project-42");
    expect(getAllByTestId("delivery-lineage-node")).toHaveLength(3);
    expect(getAllByTestId("delivery-authority").map((node) => node.textContent)).toEqual([
      "developer",
      "qa",
      "release_manager",
    ]);
    expect(getAllByTestId("delivery-digest")[0].textContent).not.toContain(DIGEST);
    expect(getAllByTestId("delivery-cost")[0].textContent).toContain("$1.25");
    expect(getByTestId("delivery-no-blockers")).toBeTruthy();
  });

  it("rejects malformed lineage instead of rendering partial authority data", () => {
    const value = snapshot();
    value.nodes[0].digest = "not-a-digest";
    const { getByTestId, queryByTestId } = render(() =>
      DeliveryView({ snapshot: value }),
    );
    expect(getByTestId("delivery-invalid").textContent).toContain("invalid digest");
    expect(queryByTestId("delivery-lineage")).toBeNull();
  });

  it("redacts credential-shaped values, internal addresses and local paths", () => {
    const value = snapshot();
    value.projectId = "secret=project-key";
    value.nodes[0].label = "token=abc123 from 10.0.0.240";
    value.blockers = ["read /work/company/private.txt"];
    const { container } = render(() => DeliveryView({ snapshot: value }));
    const rendered = container.textContent ?? "";
    expect(rendered).not.toContain("project-key");
    expect(rendered).not.toContain("abc123");
    expect(rendered).not.toContain("10.0.0.240");
    expect(rendered).not.toContain("/work/company/private.txt");
    expect(rendered).toContain("[redacted]");
    expect(rendered).toContain("[internal-address]");
    expect(rendered).toContain("[internal-path]");
  });
});
