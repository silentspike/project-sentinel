import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render } from "@solidjs/testing-library";
import { CustomerWorkspace } from "../src/customer/CustomerWorkspace";
import { customerFetch, type Overview } from "../src/customer/api";

vi.mock("../src/customer/api", async importOriginal => ({
  ...await importOriginal<typeof import("../src/customer/api")>(), customerFetch: vi.fn(),
}));

const delivery = {
  delivery: { id: "delivery-one", generation: 1, digest: "a".repeat(64) },
  release: { id: "release-one", generation: 1, digest: "b".repeat(64) },
  state: "delivered", release_state: "active", issued_at_ms: Date.now(),
  expires_at_ms: Date.now() + 60_000, preview_digest: "c".repeat(64),
};
const overview: Overview = {
  requests: [{ request_id: "request-one", summary_ref: "Studio", desired_outcome: "Website",
    constraints: [], state: "delivery_candidate", version: 1, proposal_ids: [], clarifications: [], feedback: [] }],
  proposals: [{ proposal_id: "proposal-one", request_id: "request-one", proposal_digest: "f".repeat(64), scope: "Website",
    deliverables: ["Three pages"], exclusions: [], assumptions: [], acceptance_criteria: ["Usable preview"],
    cost_ceiling_micros: 1_000_000, expires_at_unix_ms: Date.now() + 60_000 }],
  projects: [{ project_id: "project-one", request_id: "request-one", state: "delivery_candidate",
    version: 1, work_items: [], deliveries: [delivery] }],
};

afterEach(() => { cleanup(); vi.useRealTimers(); vi.resetAllMocks(); localStorage.clear(); });

describe("customer workspace preview lifecycle", () => {
  it("preserves the loaded preview and focus through identical and unrelated overview refreshes", async () => {
    vi.useFakeTimers();
    let value = structuredClone(overview);
    vi.mocked(customerFetch).mockImplementation(async (path, body) => {
      if (path === "status") return { authenticated: true, identity: {
        schema_version: 1, principal_id: "customer", tenant_id: "tenant", customer_id: "customer-one",
      } } as never;
      if (path === "overview") return structuredClone(value) as never;
      if (path === "preview") {
        const inventory = { project_id: "project-one", delivery: delivery.delivery, release: delivery.release,
          manifest_digest: "d".repeat(64), artifacts: [{ artifact_id: "source_tree", digest: "e".repeat(64), media_type: "application/json" }] };
        return (body && "file" in (body as object)
          ? { ...inventory, artifact_id: "source_tree", path: "index.html", encoding: "base64", size_bytes: 4, content: "PHAvPg==" }
          : inventory) as never;
      }
      throw new Error(`unexpected_path_${path}`);
    });
    const view = render(CustomerWorkspace);
    await vi.advanceTimersByTimeAsync(0);
    fireEvent.click(view.getByRole("button", { name: "Vorschau oeffnen" }));
    await vi.advanceTimersByTimeAsync(0);
    fireEvent.click(view.getByRole("button", { name: "Vorschau laden" }));
    await vi.advanceTimersByTimeAsync(0);
    const frame = view.getByTitle("Isolierte Lieferungsvorschau");
    const field = view.getByLabelText("HTML-Datei");
    field.focus();
    const detached: Node[] = [];
    const observer = new MutationObserver(records => records.forEach(record => detached.push(...record.removedNodes)));
    observer.observe(view.container, { childList: true, subtree: true });
    await vi.advanceTimersByTimeAsync(4_000);
    value.projects!.push({ project_id: "unrelated", request_id: "another", state: "working", version: 2, work_items: [] });
    await vi.advanceTimersByTimeAsync(2_000);
    observer.disconnect();
    expect(frame.isConnected).toBe(true);
    expect(view.getByTitle("Isolierte Lieferungsvorschau")).toBe(frame);
    expect(detached.some(node => node === frame || node.contains(frame))).toBe(false);
    expect(document.activeElement).toBe(field);
    expect(vi.mocked(customerFetch).mock.calls.filter(([path]) => path === "preview")).toHaveLength(2);
    value.projects![0].deliveries![0].release_state = "revoked";
    await vi.advanceTimersByTimeAsync(2_000);
    expect(view.queryByTitle("Isolierte Lieferungsvorschau")).toBeNull();
  });
});
