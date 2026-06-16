import { describe, it, expect, vi, afterEach } from "vitest";
import { render, fireEvent, waitFor } from "@solidjs/testing-library";
import type { BuildingConfig, DaemonParams } from "../src/api";
import { ConfigEditorView } from "../src/views/ConfigEditorView";

const building: BuildingConfig = {
  building: { name: "PixelPerfekt", address: "Strasse 1", floors: 2 },
  rooms: [
    {
      id: "buero",
      name: "Buero",
      floor: 0,
      capacity: 10,
      room_type: "office",
      adjacent: ["flur"],
      department: null,
      has_coffee_machine: true,
      has_printer: false,
    },
    {
      id: "flur",
      name: "Flur",
      floor: 0,
      capacity: 5,
      room_type: "transit",
      adjacent: ["buero"],
      department: null,
      has_coffee_machine: false,
      has_printer: false,
    },
  ],
};

const daemonParams: DaemonParams = {
  content: "[daemon]\nmax_agents = 60\ntime_scale = 1.0\ntick_rate_ms = 1000\n",
  max_agents: 60,
  time_scale: 1.0,
  tick_rate_ms: 1000,
};

function mockFetch(): void {
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: string) => {
      const u = String(url);
      if (u.includes("/api/config/rooms")) return { ok: true, json: async () => building };
      if (u.includes("/api/config/daemon")) return { ok: true, json: async () => daemonParams };
      return { ok: false, json: async () => ({ error: "unknown" }) };
    }),
  );
}

afterEach(() => vi.unstubAllGlobals());

describe("ConfigEditorView (#423)", () => {
  it("renders the editor shell with both tabs", () => {
    const { getByTestId } = render(ConfigEditorView);
    expect(getByTestId("view-config-editor")).toBeTruthy();
    expect(getByTestId("ce-tab-rooms")).toBeTruthy();
    expect(getByTestId("ce-tab-daemon")).toBeTruthy();
  });

  it("loads rooms (valid bidirectional adjacency -> no errors, save disabled until dirty)", async () => {
    mockFetch();
    const { getByTestId, queryByTestId } = render(ConfigEditorView);
    const save = (await waitFor(() => getByTestId("ce-save"))) as HTMLButtonElement;
    // bidirectional fixture -> no adjacency errors shown
    expect(queryByTestId("ce-adj-errors")).toBeNull();
    // not dirty yet -> disabled
    expect(save.disabled).toBe(true);
  });

  it("daemon tab shows the parsed params read-only", async () => {
    mockFetch();
    const { getByTestId } = render(ConfigEditorView);
    await waitFor(() => getByTestId("ce-save")); // rooms loaded
    fireEvent.click(getByTestId("ce-tab-daemon"));
    // daemon fetch is awaited after rooms -> poll the value, not just element existence.
    await waitFor(() => expect(getByTestId("ce-daemon-max-agents").textContent).toContain("60"));
    // there is no save button on the daemon tab (read-only viewer)
    expect((getByTestId("view-config-editor") as HTMLElement).querySelector('[data-testid="ce-save"]')).toBeNull();
  });
});
