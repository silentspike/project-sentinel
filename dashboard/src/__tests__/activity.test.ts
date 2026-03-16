import { afterEach, describe, expect, it } from "bun:test";
import { parseHTML } from "linkedom";
import path from "node:path";
import { pathToFileURL } from "node:url";

const ACTIVITY_MODULE_URL = pathToFileURL(
  path.resolve(import.meta.dir, "../../public/js/activity.js"),
).href;

function installDom() {
  const { document, window } = parseHTML(`
    <!doctype html>
    <html>
      <body>
        <section id="view-activity" class="view active"></section>
      </body>
    </html>
  `);

  Object.assign(globalThis, {
    window,
    document,
    Event: window.Event,
    MouseEvent: window.MouseEvent,
    HTMLElement: window.HTMLElement,
    Node: window.Node,
  });

  return { document, window };
}

async function loadActivityModule() {
  return import(`${ACTIVITY_MODULE_URL}?test=${Date.now()}-${Math.random()}`);
}

async function flushUi() {
  await Promise.resolve();
  await new Promise((resolve) => setTimeout(resolve, 0));
  await Promise.resolve();
}

function createClickEvent(): Event {
  return new Event("click", { bubbles: true, cancelable: true });
}

const EVENTS = [
  {
    id: 1,
    event_type: "room_physics_updated",
    agent_id: "buero-design-1",
    summary: "Raum buero-design-1 Physik",
    detail: "29.9°C CO2:420ppm",
    room: "buero-design-1",
    tick: 2276,
    timestamp_ms: 1,
  },
  {
    id: 2,
    event_type: "agent_action_received",
    agent_id: "18",
    summary: "Robin Krause: *faechelt sich mit der Hand Luft zu*",
    detail: "Emote",
    room: "buero-design-1",
    tick: 2277,
    timestamp_ms: 2,
  },
  {
    id: 3,
    event_type: "chaos_triggered",
    agent_id: "buero-design-1",
    summary: "Chaos: PhoneRing",
    detail: "Telefon klingelt",
    room: "buero-design-1",
    tick: 2275,
    timestamp_ms: 3,
  },
  {
    id: 4,
    event_type: "bio_state_updated",
    agent_id: "18",
    summary: "Robin Krause Bio-Update",
    detail: "H:28% E:81% S:47%",
    room: "buero-design-1",
    tick: 2278,
    timestamp_ms: 4,
  },
];

afterEach(() => {
  delete (globalThis as Record<string, unknown>).window;
  delete (globalThis as Record<string, unknown>).document;
  delete (globalThis as Record<string, unknown>).Event;
  delete (globalThis as Record<string, unknown>).MouseEvent;
  delete (globalThis as Record<string, unknown>).HTMLElement;
  delete (globalThis as Record<string, unknown>).Node;
  delete (globalThis as Record<string, unknown>).fetch;
});

describe("activity.js", () => {
  it("filters high-frequency events and supports text search", async () => {
    const { document, window } = installDom();

    globalThis.fetch = ((async () =>
      new Response(JSON.stringify(EVENTS), {
        status: 200,
        headers: { "Content-Type": "application/json" },
      })) as unknown) as typeof fetch;

    const { renderActivity } = await loadActivityModule();
    await renderActivity();
    await flushUi();

    expect(document.getElementById("activity-count")?.textContent).toBe("4 Events");
    expect(document.querySelectorAll(".activity-item")).toHaveLength(4);

    const reactionFilter = [...document.querySelectorAll(".activity-filter-btn")].find(
      (el) => el.textContent === "Reaktionen",
    );
    reactionFilter?.dispatchEvent(createClickEvent());
    await flushUi();

    const filteredText = document.getElementById("activity-list")?.textContent ?? "";
    expect(document.getElementById("activity-count")?.textContent).toBe("2 / 4 Events");
    expect(filteredText).toContain("Robin Krause: *faechelt sich mit der Hand Luft zu*");
    expect(filteredText).toContain("Chaos: PhoneRing");
    expect(filteredText).not.toContain("Raum buero-design-1 Physik");
    expect(filteredText).not.toContain("Robin Krause Bio-Update");

    const search = document.getElementById("activity-search") as HTMLInputElement | null;
    expect(search).toBeTruthy();
    if (!search) return;
    search.value = "telefon";
    search.dispatchEvent(new window.Event("input", { bubbles: true }));
    await flushUi();

    const searchText = document.getElementById("activity-list")?.textContent ?? "";
    expect(document.getElementById("activity-count")?.textContent).toBe("1 / 4 Events");
    expect(searchText).toContain("Chaos: PhoneRing");
    expect(searchText).not.toContain("Robin Krause: *faechelt sich mit der Hand Luft zu*");
  });
});
