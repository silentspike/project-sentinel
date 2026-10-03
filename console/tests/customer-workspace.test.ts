import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render } from "@solidjs/testing-library";
import { CustomerWorkspace } from "../src/customer/CustomerWorkspace";
import { CustomerApiError, customerFetch, type Overview } from "../src/customer/api";

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

afterEach(() => { cleanup(); vi.useRealTimers(); vi.resetAllMocks(); vi.unstubAllGlobals(); localStorage.clear(); });

describe("customer workspace preview lifecycle", () => {
  it("renews an expired delivery through a reserved customer command without accepting it", async () => {
    vi.useFakeTimers();
    const value = structuredClone(overview);
    const receipt = value.projects![0].deliveries![0];
    receipt.issued_at_ms = Date.now() - 120_000; receipt.expires_at_ms = Date.now() - 60_000;
    const fetcher = vi.fn(async (_url: string, options: RequestInit) => {
      const envelope = JSON.parse(options.body as string);
      expect(envelope.intent.action).toBe("renew_preview");
      receipt.preview_access = { access: { id: "preview-one", generation: 2, digest: "f".repeat(64) },
        issued_at_ms: Date.now(), expires_at_ms: Date.now() + 60_000 };
      return new Response("{}");
    });
    vi.stubGlobal("fetch", fetcher);
    vi.stubGlobal("navigator", { locks: { request: async (_name: string, _options: object, callback: (lock: object) => Promise<void>) => callback({}) } });
    vi.mocked(customerFetch).mockImplementation(async path => {
      if (path === "status") return { authenticated: true, identity: {
        schema_version: 1, principal_id: "customer", tenant_id: "tenant", customer_id: "customer-one",
      } } as never;
      if (path === "overview") return structuredClone(value) as never;
      throw new Error(`unexpected_path_${path}`);
    });
    const view = render(CustomerWorkspace);
    await vi.advanceTimersByTimeAsync(0);
    expect((view.getByRole("button", { name: "Vorschau oeffnen" }) as HTMLButtonElement).disabled).toBe(true);
    fireEvent.click(view.getByRole("button", { name: "Vorschau erneuern" }));
    await vi.advanceTimersByTimeAsync(0);
    expect(fetcher).toHaveBeenCalledTimes(1);
    expect((view.getByRole("button", { name: "Vorschau oeffnen" }) as HTMLButtonElement).disabled).toBe(false);
    expect(view.queryByRole("button", { name: "Vorschau erneuern" })).toBeNull();
    expect(receipt.state).toBe("delivered");
    expect(receipt.expires_at_ms).toBeLessThan(Date.now());
    await vi.advanceTimersByTimeAsync(60_000);
    expect(view.getByRole("button", { name: "Vorschau erneuern" })).toBeDefined();
  });
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

describe("customer work progress", () => {
  const loadOverview = async (value: Overview) => {
    vi.useFakeTimers();
    vi.mocked(customerFetch).mockImplementation(async path => {
      if (path === "status") return { authenticated: true, identity: {
        schema_version: 1, principal_id: "customer", tenant_id: "tenant", customer_id: "customer-one",
      } } as never;
      if (path === "overview") return structuredClone(value) as never;
      throw new Error(`unexpected_path_${path}`);
    });
    const view = render(CustomerWorkspace);
    await vi.advanceTimersByTimeAsync(0);
    return view;
  };

  it.each([
    { status: "paused_internal_observation", label: "Paused: awaiting internal observation." },
    { status: "model_outcome_unknown", label: "Model outcome unknown." },
    { status: "paused_work_window", label: "Paused: work window unavailable." },
    { status: "tool_outcome_unknown", label: "Tool outcome unknown." },
  ] as const)("shows $status separately from company state without work actions", async progress => {
    const value = structuredClone(overview);
    value.projects![0].work_items = [{ work_item_id: "website", state: "assigned", adaptive_progress: progress }];
    const fetcher = vi.fn();
    vi.stubGlobal("fetch", fetcher);
    const view = await loadOverview(value);
    const row = view.getByText("website").closest("tr")!;
    expect([...row.querySelectorAll("td")].map(cell => cell.textContent)).toEqual([
      "website", "assigned", progress.label,
    ]);
    expect(view.getByRole("columnheader", { name: "Arbeitsstatus" })).toBeDefined();
    expect(view.getByRole("columnheader", { name: "Ausfuehrungsstatus" })).toBeDefined();
    expect(row.querySelectorAll("button, a, input")).toHaveLength(0);
    expect(view.queryByRole("button", { name: "Gleiche Anfrage erneut pruefen" })).toBeNull();
    await vi.advanceTimersByTimeAsync(2_000);
    expect(fetcher).not.toHaveBeenCalled();
    expect(vi.mocked(customerFetch).mock.calls.every(([path]) => path === "status" || path === "overview")).toBe(true);
    expect(localStorage.length).toBe(0);
  });

  it("keeps legacy and normal work compatible without inventing execution status", async () => {
    const value = structuredClone(overview);
    value.projects![0].work_items = [{ work_item_id: "website", state: "assigned" }];
    const view = await loadOverview(value);
    expect([...view.getByText("website").closest("tr")!.querySelectorAll("td")]
      .map(cell => cell.textContent)).toEqual(["website", "assigned", ""]);
    expect(view.queryByText("Model outcome unknown.")).toBeNull();
    expect(view.queryByText("Paused: awaiting internal observation.")).toBeNull();
  });

  it.each([
    { status: "model_outcome_unknown", label: "private-provider-secret /work/private token=secret" },
    { status: "provider_quota", label: "private-provider-secret /work/private token=secret" },
    { status: "paused_work_window", label: "Model outcome unknown." },
    { status: "tool_outcome_unknown", label: "Paused: work window unavailable." },
  ])("never renders unrecognized status or noncanonical label: $status", async progress => {
    const value = structuredClone(overview);
    value.projects![0].work_items = [{ work_item_id: "website", state: "assigned",
      adaptive_progress: progress as never }];
    const view = await loadOverview(value);
    expect(view.container.textContent).not.toContain("private-provider-secret");
    expect(view.container.textContent).not.toContain("/work/private");
    expect(view.container.textContent).not.toContain("token=secret");
    expect(view.getByText("website").closest("tr")!.querySelectorAll("td")[2].textContent).toBe("");
  });

  it("ignores private extras even on a recognized progress object", async () => {
    const value = structuredClone(overview);
    value.projects![0].work_items = [{ work_item_id: "website", state: "assigned", adaptive_progress: {
      status: "model_outcome_unknown", label: "Model outcome unknown.",
      reason: "private-provider-secret", workspace_path: "/work/private", credential: "token=secret",
    } as never }];
    const view = await loadOverview(value);
    expect(view.getByText("Model outcome unknown.")).toBeDefined();
    expect(view.container.textContent).not.toContain("private-provider-secret");
    expect(view.container.textContent).not.toContain("/work/private");
    expect(view.container.textContent).not.toContain("token=secret");
  });

  it("shows a fixed failed-read error without inventing adaptive progress", async () => {
    vi.useFakeTimers();
    vi.mocked(customerFetch).mockImplementation(async path => {
      if (path === "status") return { authenticated: true, identity: {
        schema_version: 1, principal_id: "customer", tenant_id: "tenant", customer_id: "customer-one",
      } } as never;
      throw new CustomerApiError(503, "Customer progress is unavailable");
    });
    const view = render(CustomerWorkspace);
    await vi.advanceTimersByTimeAsync(0);
    expect(view.getByRole("alert").textContent).toBe("Customer progress is unavailable");
    expect(view.queryByText("Model outcome unknown.")).toBeNull();
    expect(view.queryByText("Paused: awaiting internal observation.")).toBeNull();
  });

  it.each([
    { status: "paused_internal_observation", label: "Paused: awaiting internal observation." },
    { status: "model_outcome_unknown", label: "Model outcome unknown." },
    { status: "paused_work_window", label: "Paused: work window unavailable." },
    { status: "tool_outcome_unknown", label: "Tool outcome unknown." },
  ] as const)("suppresses stale $status after a 503 and restores progress only after refresh succeeds", async progress => {
    const value = structuredClone(overview);
    value.projects![0].work_items = [{ work_item_id: "website", state: "assigned", adaptive_progress: progress }];
    const view = await loadOverview(value);
    expect(view.getByText(progress.label)).toBeDefined();
    let fail = true;
    vi.mocked(customerFetch).mockImplementation(async path => {
      if (path !== "overview") throw new Error(`unexpected_path_${path}`);
      if (fail) throw new CustomerApiError(503, "Customer progress is unavailable");
      return structuredClone(value) as never;
    });
    await vi.advanceTimersByTimeAsync(2_000);
    expect(view.getByRole("alert").textContent).toBe("Customer progress is unavailable");
    expect([...view.getByText("website").closest("tr")!.querySelectorAll("td")]
      .map(cell => cell.textContent)).toEqual(["website", "assigned", ""]);
    expect(view.queryByText(progress.label)).toBeNull();
    await vi.advanceTimersByTimeAsync(2_000);
    expect(view.queryByText(progress.label)).toBeNull();
    const recovered = progress.status === "model_outcome_unknown"
      ? { status: "paused_internal_observation", label: "Paused: awaiting internal observation." } as const
      : { status: "model_outcome_unknown", label: "Model outcome unknown." } as const;
    value.projects![0].work_items[0].adaptive_progress = recovered;
    fail = false;
    fireEvent.click(view.getByRole("button", { name: "Aktualisieren" }));
    await vi.advanceTimersByTimeAsync(0);
    expect(view.getByText(recovered.label)).toBeDefined();
    expect(view.queryByText(progress.label)).toBeNull();
    expect(view.queryByRole("alert")).toBeNull();
    expect(view.getByText("website").closest("tr")!.querySelectorAll("button, a, input")).toHaveLength(0);
  });
});
