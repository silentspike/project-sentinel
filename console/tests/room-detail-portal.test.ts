import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, waitFor, within } from "@solidjs/testing-library";
import { FloorplanView } from "../src/views/FloorplanView";

vi.mock("../src/stores/console", () => ({
  consoleStore: {
    rooms: [{ room_id: "buero-qa", occupant_count: 0, transit_count: 0 }],
    agents: [],
  },
}));
vi.mock("../src/api", () => ({
  apiJson: vi.fn(async () => ({ room_id: "buero-qa", occupant_count: 0, transit_count: 0, occupants: [] })),
  postJson: vi.fn(),
}));

afterEach(cleanup);

async function openRoom() {
  const view = render(FloorplanView);
  const panel = view.getByTestId("view-floorplan");
  fireEvent.click(panel.querySelector(".room-card")!);
  await waitFor(() => expect(document.querySelector(".detail-metrics")).not.toBeNull());
  return { view, panel, drawer: document.querySelector<HTMLElement>('[data-testid="room-detail"]')! };
}

describe("room detail viewport portal", () => {
  it("keeps the dialog outside a scrolled or transformed tiling panel", async () => {
    const { panel, drawer } = await openRoom();
    expect(panel.contains(drawer)).toBe(false);
    expect(document.body.contains(drawer)).toBe(true);
    expect(drawer.getAttribute("data-room-id")).toBe("buero-qa");
    expect(drawer.getAttribute("aria-label")).toBe("QA-B\u00fcro");
    expect(panel.contains(document.querySelector(".drawer-backdrop"))).toBe(false);
  });

  it("closes the portal and clears the selected room through its close control", async () => {
    const { panel, drawer } = await openRoom();
    fireEvent.click(within(drawer).getByRole("button", { name: "Schliessen" }));
    expect(document.querySelector('[data-testid="room-detail"]')).toBeNull();
    expect(document.querySelector(".drawer-backdrop")).toBeNull();
    expect(panel.querySelector('.room-card[aria-pressed="true"]')).toBeNull();
  });

  it("removes modal content when the owning view unmounts", async () => {
    const { view } = await openRoom();
    view.unmount();
    expect(document.querySelector('[data-testid="room-detail"]')).toBeNull();
    expect(document.querySelector(".drawer-backdrop")).toBeNull();
  });
});
