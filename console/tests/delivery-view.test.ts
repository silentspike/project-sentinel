import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { fireEvent, render, waitFor } from "@solidjs/testing-library";
import { DeliveryView } from "../src/views/DeliveryView";
import {
  DELIVERY_LINEAGE_DEADLINE_MS,
  fetchPublicDeliveryLineage,
} from "../src/views/delivery/api";
import {
  parsePublicDeliveryLineageDto,
  validateLineage,
  type PublicDeliveryLineageDto,
} from "../src/views/delivery/lineage";

const DIGEST = "a".repeat(64);

function snapshot(): PublicDeliveryLineageDto {
  return {
    schemaVersion: 1,
    serverRedacted: true,
    projectLabel: "project-42",
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
        costMinor: "125",
        currency: "USD",
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
    authorityGeneration: 7,
    readAtMs: 1_775_000_000_000,
  };
}

afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
  window.history.replaceState({}, "", "/");
});

function setDeliveryScope(): void {
  window.history.replaceState({}, "", "/?tenant_id=m0-company&project_id=project-42");
}

describe("delivery lineage model", () => {
  it("does not fetch missing, invalid, oversized or already cancelled scopes", async () => {
    const fetchMock = vi.fn();
    vi.stubGlobal("fetch", fetchMock);
    for (const tenantId of ["", "a/b", "a".repeat(129)]) {
      await expect(fetchPublicDeliveryLineage(undefined, { tenantId, projectId: "project-42" })).rejects.toThrow();
    }
    const cancelled = new AbortController();
    cancelled.abort();
    await expect(fetchPublicDeliveryLineage(cancelled.signal, { tenantId: "tenant", projectId: "project" })).rejects.toThrow();
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("uses explicit scope instead of URL scope with encoded query parameters", async () => {
    setDeliveryScope();
    const fetchMock = vi.fn().mockResolvedValue(new Response(JSON.stringify(snapshot()), {
      headers: { "content-type": "application/json" },
    }));
    vi.stubGlobal("fetch", fetchMock);
    await fetchPublicDeliveryLineage(undefined, { tenantId: "tenant.test_1", projectId: "project-2" });
    expect(fetchMock).toHaveBeenCalledWith(
      `/api/v1/delivery/lineage?tenant_id=${encodeURIComponent("tenant.test_1")}&project_id=${encodeURIComponent("project-2")}`,
      expect.objectContaining({ method: "GET", credentials: "include" }),
    );
  });

  it.each([[404, "inaccessible"], [401, "access"], [403, "access"], [503, "unavailable"]])(
    "classifies HTTP %s without exposing the response body", async (status, kind) => {
      setDeliveryScope();
      vi.stubGlobal("fetch", vi.fn().mockResolvedValue(new Response("private upstream details", { status: Number(status) })));
      await expect(fetchPublicDeliveryLineage()).rejects.toMatchObject({ kind, message: "Delivery lineage is unavailable" });
    },
  );
  it("validates the server-redacted digest-bound DTO", () => {
    const value = snapshot();
    expect(validateLineage(value)).toEqual([]);
    expect("tenantId" in value).toBe(false);

    value.edges.push({ from: "release-1", to: "missing" });
    expect(validateLineage(value)).toContain("dangling edge: release-1->missing");
  });

  it("rejects duplicate, cyclic and disconnected public lineage graphs", () => {
    const duplicate = snapshot();
    duplicate.edges.push({ ...duplicate.edges[0] });
    expect(validateLineage(duplicate)).toContain("duplicate edge: candidate-1->qa-1");

    const cyclic = snapshot();
    cyclic.edges.push({ from: "release-1", to: "candidate-1" });
    expect(validateLineage(cyclic)).toContain("cyclic lineage graph");

    const disconnected = snapshot();
    disconnected.edges.pop();
    expect(validateLineage(disconnected)).toContain("disconnected lineage graph");
  });

  it("rejects unknown fields before raw authority data reaches the view", () => {
    const value = { ...snapshot(), tenantId: "tenant-private" };
    expect(() => parsePublicDeliveryLineageDto(value)).toThrow("missing or unknown fields");
  });

  it("rejects non-boolean readiness and unsupported currency semantics", () => {
    expect(() =>
      parsePublicDeliveryLineageDto({ ...snapshot(), adapterReady: "yes" }),
    ).toThrow("adapterReady is not a boolean");
    expect(() =>
      parsePublicDeliveryLineageDto({
        ...snapshot(),
        nodes: [{ ...snapshot().nodes[0], costMinor: "125", currency: "JPY" }],
        edges: [],
      }),
    ).toThrow("invalid cost");
  });

  it("accepts every complete workflow, evidence, manifest and delivery lineage class", () => {
    const stages = [
      "customer_request", "agreement", "project", "work_item", "participant",
      "decision", "handoff", "blocker", "candidate", "qa", "workbench", "artifact",
      "review", "test", "finding", "approval", "manifest", "release", "delivery",
      "acceptance", "closeout", "rollback",
    ] as const;
    const value = snapshot();
    value.nodes = stages.map((stage, index) => ({
      id: `node-${index + 1}`,
      stage,
      label: `Public ${stage.replaceAll("_", " ")}`,
      state: "recorded",
      digest: index.toString(16).padStart(64, "a").slice(-64),
      generation: 1,
      actorRole: stage === "participant" ? "developer" : "gaia_observer",
    }));
    value.edges = value.nodes.slice(1).map((node, index) => ({
      from: value.nodes[index].id,
      to: node.id,
    }));
    expect(parsePublicDeliveryLineageDto(value).nodes.map((node) => node.stage)).toEqual(stages);
  });

  it("fetches only the authenticated server-redacted DTO contract", async () => {
    setDeliveryScope();
    const fetchMock = vi.fn().mockResolvedValue(
      new Response(JSON.stringify(snapshot()), {
        status: 200,
        headers: { "content-type": "application/json" },
      }),
    );
    vi.stubGlobal("fetch", fetchMock);

    await expect(fetchPublicDeliveryLineage()).resolves.toEqual(snapshot());
    expect(fetchMock).toHaveBeenCalledWith(
      "/api/v1/delivery/lineage?tenant_id=m0-company&project_id=project-42",
      expect.objectContaining({ method: "GET", credentials: "include" }),
    );
  });

  it("rejects oversized declared and streamed bodies before decoding", async () => {
    setDeliveryScope();
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValueOnce(
        new Response("{}", {
          status: 200,
          headers: {
            "content-type": "application/json",
            "content-length": String(256 * 1024 + 1),
          },
        }),
      ),
    );
    await expect(fetchPublicDeliveryLineage()).rejects.toThrow(
      "Delivery lineage is unavailable",
    );

    const oversizedChunk = new Uint8Array(256 * 1024 + 1);
    const cancel = vi.fn();
    const body = new ReadableStream<Uint8Array>({
      pull(controller) {
        controller.enqueue(oversizedChunk);
      },
      cancel,
    });
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(
        new Response(body, {
          status: 200,
          headers: { "content-type": "application/json" },
        }),
      ),
    );
    await expect(fetchPublicDeliveryLineage()).rejects.toThrow(
      "Delivery lineage is unavailable",
    );
    expect(cancel).toHaveBeenCalledOnce();
  });

  it("accepts exact JSON media types and rejects JSONP", async () => {
    setDeliveryScope();
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(
        new Response(JSON.stringify(snapshot()), {
          status: 200,
          headers: { "content-type": "application/json; charset=utf-8" },
        }),
      ),
    );
    await expect(fetchPublicDeliveryLineage()).resolves.toEqual(snapshot());

    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(
        new Response("callback({})", {
          status: 200,
          headers: { "content-type": "application/jsonp" },
        }),
      ),
    );
    await expect(fetchPublicDeliveryLineage()).rejects.toThrow(
      "Delivery lineage is unavailable",
    );
  });

  it("aborts stalled fetches and bodies at the internal deadline without leaking timers", async () => {
    setDeliveryScope();
    vi.useFakeTimers();
    vi.stubGlobal(
      "fetch",
      vi.fn((_input: RequestInfo | URL, init?: RequestInit) =>
        new Promise<Response>((_resolve, reject) => {
          init?.signal?.addEventListener("abort", () => reject(new Error("aborted")), {
            once: true,
          });
        }),
      ),
    );
    const stalledFetch = fetchPublicDeliveryLineage();
    const stalledFetchResult = expect(stalledFetch).rejects.toThrow(
      "Delivery lineage is unavailable",
    );
    await vi.advanceTimersByTimeAsync(DELIVERY_LINEAGE_DEADLINE_MS);
    await stalledFetchResult;
    expect(vi.getTimerCount()).toBe(0);

    const cancel = vi.fn();
    const stalledBody = new ReadableStream<Uint8Array>({
      pull: () => new Promise<void>(() => undefined),
      cancel,
    });
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(
        new Response(stalledBody, {
          status: 200,
          headers: { "content-type": "application/json" },
        }),
      ),
    );
    const pendingBody = fetchPublicDeliveryLineage();
    const pendingBodyResult = expect(pendingBody).rejects.toThrow(
      "Delivery lineage is unavailable",
    );
    await Promise.resolve();
    await vi.advanceTimersByTimeAsync(DELIVERY_LINEAGE_DEADLINE_MS);
    await pendingBodyResult;
    expect(cancel).toHaveBeenCalledOnce();
    expect(vi.getTimerCount()).toBe(0);
  });

  it("combines caller cancellation with its deadline and cleans up the timeout", async () => {
    setDeliveryScope();
    vi.useFakeTimers();
    const controller = new AbortController();
    vi.stubGlobal(
      "fetch",
      vi.fn((_input: RequestInfo | URL, init?: RequestInit) =>
        new Promise<Response>((_resolve, reject) => {
          init?.signal?.addEventListener("abort", () => reject(new Error("aborted")), {
            once: true,
          });
        }),
      ),
    );
    const pending = fetchPublicDeliveryLineage(controller.signal);
    const pendingResult = expect(pending).rejects.toThrow("Delivery lineage is unavailable");
    controller.abort();
    await pendingResult;
    expect(vi.getTimerCount()).toBe(0);
  });
});

describe("DeliveryView", () => {
  it("distinguishes an injected loader read error from adapter readiness", async () => {
    const { getByTestId } = render(() =>
      DeliveryView({ load: async () => Promise.reject(new Error("unavailable")) }),
    );
    await waitFor(() =>
      expect(getByTestId("delivery-adapter-state").textContent).toContain("Read error"),
    );
    expect(getByTestId("delivery-read-error")).toBeTruthy();
  });

  it.each(["/", "/?tenant_id=tenant&project_id=bad%2Fscope"])("requires valid explicit scope without GET at %s", async (url) => {
    window.history.replaceState({}, "", url);
    const fetchMock = vi.fn();
    vi.stubGlobal("fetch", fetchMock);
    const view = render(() => DeliveryView({}));
    expect(view.getByTestId("delivery-select-project").textContent).toBe(url === "/" ? "Select project" : "Invalid scope");
    fireEvent.submit(view.getByRole("button", { name: "Load" }).closest("form")!);
    await Promise.resolve();
    expect(fetchMock).not.toHaveBeenCalled();
    expect(view.queryByTestId("delivery-unavailable")).toBeNull();
  });

  it("loads only on explicit submission and supports initial URL scope", async () => {
    const fetchMock = vi.fn().mockImplementation(() => Promise.resolve(new Response(JSON.stringify(snapshot()), {
      headers: { "content-type": "application/json" },
    })));
    vi.stubGlobal("fetch", fetchMock);
    const view = render(() => DeliveryView({}));
    fireEvent.input(view.getByLabelText("Tenant"), { target: { value: "tenant.test" } });
    fireEvent.input(view.getByLabelText("Project"), { target: { value: "project-42" } });
    expect(view.getByTestId("delivery-adapter-state").textContent).toBe("Ready to load");
    expect(view.getByRole("button", { name: "Load" }).hasAttribute("disabled")).toBe(false);
    expect(fetchMock).not.toHaveBeenCalled();
    fireEvent.click(view.getByRole("button", { name: "Load" }));
    await waitFor(() => expect(view.getByTestId("delivery-project")).toBeTruthy());
    expect(fetchMock).toHaveBeenCalledWith("/api/v1/delivery/lineage?tenant_id=tenant.test&project_id=project-42", expect.anything());
    view.unmount();
    setDeliveryScope();
    const initial = render(() => DeliveryView({}));
    await waitFor(() => expect(initial.getByTestId("delivery-project")).toBeTruthy());
    expect(fetchMock).toHaveBeenCalledTimes(2);
  });

  it("aborts superseded loads and ignores late results even if the loader ignores abort", async () => {
    setDeliveryScope();
    let first!: (value: PublicDeliveryLineageDto) => void;
    let second!: (value: PublicDeliveryLineageDto) => void;
    const signals: AbortSignal[] = [];
    const load = vi.fn((signal: AbortSignal) => {
      signals.push(signal);
      return new Promise<PublicDeliveryLineageDto>((resolve) => {
        if (signals.length === 1) first = resolve; else second = resolve;
      });
    });
    const view = render(() => DeliveryView({ load }));
    expect(view.getByTestId("delivery-loading")).toBeTruthy();
    fireEvent.input(view.getByLabelText("Project"), { target: { value: "project-new" } });
    expect(signals[0].aborted).toBe(true);
    fireEvent.click(view.getByRole("button", { name: "Load" }));
    second({ ...snapshot(), projectLabel: "project-new" });
    await waitFor(() => expect(view.getByTestId("delivery-project").textContent).toBe("project-new"));
    first(snapshot());
    await Promise.resolve();
    expect(view.getByTestId("delivery-project").textContent).toBe("project-new");
    view.unmount();
    expect(signals[1].aborted).toBe(true);
  });

  it("aborts pending requests on cleanup", () => {
    let signal!: AbortSignal;
    const view = render(() => DeliveryView({ load: (value) => {
      signal = value;
      return new Promise(() => undefined);
    } }));
    view.unmount();
    expect(signal.aborted).toBe(true);
  });

  it.each([[404, "No accessible delivery lineage"], [401, "Access unavailable"], [403, "Access unavailable"], [503, "Read error"]])(
    "shows fixed HTTP %s state and permits retry", async (status, label) => {
      setDeliveryScope();
      const fetchMock = vi.fn().mockResolvedValueOnce(new Response("secret upstream", { status: Number(status) }))
        .mockResolvedValueOnce(new Response(JSON.stringify(snapshot()), { headers: { "content-type": "application/json" } }));
      vi.stubGlobal("fetch", fetchMock);
      const view = render(() => DeliveryView({}));
      await waitFor(() => expect(view.getByTestId("delivery-read-error").textContent).toBe(label));
      expect(view.container.textContent).not.toContain("secret upstream");
      fireEvent.click(view.getByRole("button", { name: "Load" }));
      await waitFor(() => expect(view.getByTestId("delivery-project")).toBeTruthy());
      expect(fetchMock).toHaveBeenCalledTimes(2);
    },
  );

  it("rejects empty lineage without treating it as an unavailable adapter", () => {
    const view = render(() => DeliveryView({ snapshot: { ...snapshot(), nodes: [], edges: [] } }));
    expect(view.getByTestId("delivery-invalid").textContent).toContain("empty lineage graph");
    expect(view.queryByTestId("delivery-lineage")).toBeNull();
    expect(view.getByTestId("delivery-adapter-state").textContent).toBe("Adapter ready");
    expect(view.queryByTestId("delivery-unavailable")).toBeNull();
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
    expect(getByTestId("delivery-project").textContent).not.toContain("Project project");
    expect(getAllByTestId("delivery-lineage-node")).toHaveLength(3);
    expect(getAllByTestId("delivery-lineage-node").every((node) => node.classList.contains("delivery-lineage-node"))).toBe(true);
    expect(getAllByTestId("delivery-authority").map((node) => node.textContent)).toEqual([
      "developer",
      "qa",
      "release_manager",
    ]);
    expect(getAllByTestId("delivery-digest")[0].textContent).not.toContain(DIGEST);
    expect(getAllByTestId("delivery-cost")[0].textContent).toContain("USD 1.25");
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

  it("rejects DTOs containing credential-shaped values or internal identifiers", () => {
    const value = snapshot();
    value.projectLabel = "secret=project-key";
    value.nodes[0].label = "token=abc123 from 10.0.0.240";
    value.blockers = ["read /work/company/private.txt"];
    const { getByTestId, queryByTestId } = render(() =>
      DeliveryView({ snapshot: value }),
    );
    expect(getByTestId("delivery-invalid").textContent).toContain(
      "forbidden sensitive or internal text",
    );
    expect(queryByTestId("delivery-lineage")).toBeNull();
  });
});

describe("Delivery navigation", () => {
  let App: typeof import("../src/App").default;
  beforeAll(async () => {
    vi.resetModules();
    vi.doMock("../src/auth", () => ({
      authStatus: async () => true,
      login: async () => "ok",
    }));
    vi.doMock("../src/stores/console", async (importOriginal) => ({
      ...(await importOriginal<typeof import("../src/stores/console")>()),
      connectTransport: vi.fn(),
    }));
    vi.stubGlobal("fetch", vi.fn().mockRejectedValue(new Error("unavailable")));
    vi.stubGlobal("matchMedia", () => ({
      matches: false,
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
    }));
    vi.stubGlobal(
      "ResizeObserver",
      class {
        observe() {}
        disconnect() {}
      },
    );
    Object.defineProperty(Element.prototype, "animate", {
      configurable: true,
      value: vi.fn(() => ({ cancel: vi.fn() })),
    });
    App = (await import("../src/App")).default;
  });

  it("opens the unscoped product surface with project selection", async () => {
    const { getByTestId } = render(() => App());

    await waitFor(() => expect(getByTestId("open-delivery")).toBeTruthy());
    fireEvent.click(getByTestId("open-delivery"));
    expect(getByTestId("delivery-product-surface")).toBeTruthy();
    await waitFor(() =>
      expect(getByTestId("delivery-adapter-state").textContent).toContain("Select project"),
    );
  });
});
