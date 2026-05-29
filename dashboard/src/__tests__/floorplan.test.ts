import { afterEach, describe, expect, it } from "bun:test";
import { parseHTML } from "linkedom";
import path from "node:path";
import { pathToFileURL } from "node:url";

const FLOORPLAN_MODULE_URL = pathToFileURL(
  path.resolve(import.meta.dir, "../../public/js/floorplan.js"),
).href;

// auth.js wird (ohne Cache-Buster) als geteilte Modul-Instanz geladen — dieselbe,
// die floorplan.js via relativem Import nutzt. setTestAuth setzt das UI-Auth-Flag
// auf exakt der Instanz, aus der floorplan.js isAuthenticated() liest (#402).
const AUTH_MODULE_URL = pathToFileURL(
  path.resolve(import.meta.dir, "../../public/js/auth.js"),
).href;

async function setTestAuth(value: boolean): Promise<void> {
  const mod = await import(AUTH_MODULE_URL);
  mod._setAuthenticated(value);
}

class StorageMock {
  private values = new Map<string, string>();

  getItem(key: string): string | null {
    return this.values.has(key) ? this.values.get(key)! : null;
  }

  setItem(key: string, value: string): void {
    this.values.set(key, String(value));
  }

  removeItem(key: string): void {
    this.values.delete(key);
  }

  clear(): void {
    this.values.clear();
  }
}

function installDom() {
  const { document, window } = parseHTML(`
    <!doctype html>
    <html>
      <body>
        <section id="view-floorplan" class="view active"></section>
      </body>
    </html>
  `);

  const storage = new StorageMock();
  Object.assign(globalThis, {
    window,
    document,
    Event: window.Event,
    MouseEvent: window.MouseEvent,
    HTMLElement: window.HTMLElement,
    Node: window.Node,
    sessionStorage: storage,
  });

  return { document, window, storage };
}

function createClickEvent(): Event {
  return new Event("click", { bubbles: true, cancelable: true });
}

async function loadFloorplanModule() {
  return import(`${FLOORPLAN_MODULE_URL}?test=${Date.now()}-${Math.random()}`);
}

async function flushUi() {
  await Promise.resolve();
  await new Promise((resolve) => setTimeout(resolve, 0));
  await Promise.resolve();
}

const ROOMS = [
  {
    id: "buero-design-2",
    name: "Designbüro 2",
    floor: 1,
    capacity: 6,
    room_type: "office",
    occupant_count: 2,
    transit_count: 0,
    active_chaos: { event_type: "PrinterBroken", description: "Drucker defekt" },
    active_smells: null,
    temperature: 22.2,
    co2_ppm: 440,
    noise_db: 30,
    last_event_tick: 120,
    occupants: ["Anna Schmidt", "Max Weber"],
  },
];

const INITIAL_DETAIL = {
  ...ROOMS[0],
  physics_history: [
    {
      tick: 100,
      timestamp_ms: 1000,
      temperature: 22.0,
      co2_ppm: 430,
      noise_db: 30,
      occupant_count: 2,
    },
  ],
  chaos_history: [
    {
      id: 1,
      event_id: "chaos-1",
      chaos_type: "PrinterBroken",
      room_id: "buero-design-2",
      description: "Drucker defekt",
      tick: 118,
      timestamp_ms: 1001,
    },
  ],
  stimulus_history: [
    {
      event_id: "stim-1",
      room_id: "buero-design-2",
      stimulus_type: "co2",
      delta: 900,
      description: "CO2-Reiz +900 ppm",
      tick: 119,
      timestamp_ms: 1001,
    },
  ],
  recent_reactions: [
    {
      event_id: "action-1",
      agent_id: "1",
      agent_name: "Anna Schmidt",
      action_type: "ToolUse",
      content: "Die Luft ist stickig hier.",
      target_room: "buero-design-2",
      tick: 121,
      timestamp_ms: 1002,
      correlation_id: "corr-1",
      chaos_event_id: "chaos-1",
      chaos_type: "PrinterBroken",
      chaos_description: "Drucker defekt",
      chaos_tick: 118,
      stimulus_event_id: "stim-1",
      stimulus_type: "co2",
      stimulus_description: "CO2-Reiz +900 ppm",
      stimulus_tick: 119,
    },
  ],
  reaction_window_ticks: 60,
};

const UPDATED_DETAIL = {
  ...INITIAL_DETAIL,
  co2_ppm: 1340,
  active_chaos: { event_type: "AirConBroken", description: "Klimaanlage defekt" },
  chaos_history: [
    {
      id: 2,
      event_id: "chaos-2",
      chaos_type: "AirConBroken",
      room_id: "buero-design-2",
      description: "Klimaanlage defekt",
      tick: 140,
      timestamp_ms: 1010,
    },
    ...INITIAL_DETAIL.chaos_history,
  ],
  recent_reactions: [
    {
      ...INITIAL_DETAIL.recent_reactions[0],
      content: "Mir ist zu warm hier.",
      stimulus_event_id: "stim-2",
      stimulus_type: "temperature",
      stimulus_description: "Temperaturreiz +4.0 °C",
      stimulus_tick: 141,
      chaos_event_id: "chaos-2",
      chaos_type: "AirConBroken",
      chaos_description: "Klimaanlage defekt",
      chaos_tick: 140,
    },
  ],
  stimulus_history: [
    {
      event_id: "stim-2",
      room_id: "buero-design-2",
      stimulus_type: "temperature",
      delta: 4,
      description: "Temperaturreiz +4.0 °C",
      tick: 141,
      timestamp_ms: 1012,
    },
    ...INITIAL_DETAIL.stimulus_history,
  ],
};

afterEach(async () => {
  // In-Memory-Key persistiert ueber Tests (anders als der per-Test frische
  // sessionStorage-Mock) — daher explizit zuruecksetzen.
  await setTestAuth(false);
  delete (globalThis as Record<string, unknown>).window;
  delete (globalThis as Record<string, unknown>).document;
  delete (globalThis as Record<string, unknown>).Event;
  delete (globalThis as Record<string, unknown>).MouseEvent;
  delete (globalThis as Record<string, unknown>).HTMLElement;
  delete (globalThis as Record<string, unknown>).Node;
  delete (globalThis as Record<string, unknown>).sessionStorage;
  delete (globalThis as Record<string, unknown>).fetch;
});

describe("floorplan.js", () => {
  it("renders clickable room cards and opens the detail drawer", async () => {
    const { document, window } = installDom();
    const fetchCalls: string[] = [];

    globalThis.fetch = (async (input: RequestInfo | URL) => {
      fetchCalls.push(String(input));
      return new Response(JSON.stringify(INITIAL_DETAIL), {
        status: 200,
        headers: { "Content-Type": "application/json" },
      });
    }) as typeof fetch;

    const { renderFloorplan } = await loadFloorplanModule();
    renderFloorplan(ROOMS);

    const card = document.querySelector('[data-room-id="buero-design-2"]');
    expect(card?.localName).toBe("button");
    expect(card?.getAttribute("aria-pressed")).toBe("false");

    card?.dispatchEvent(createClickEvent());
    await flushUi();
    await flushUi();

    expect(fetchCalls[0]).toContain("/api/rooms/buero-design-2/detail");
    expect(document.querySelector(".room-detail-drawer")).toBeTruthy();
    expect(document.querySelector(".room-detail-title")?.textContent).toContain("Designbüro 2");
    expect(document.querySelector('[data-room-id="buero-design-2"]')?.getAttribute("aria-pressed")).toBe(
      "true",
    );
    expect(
      [...document.querySelectorAll(".room-detail-heading")].map((el) => el.textContent),
    ).toEqual([
      "Snapshot",
      "Physics-Verlauf",
      "Raumreiz testen",
      "Chaos-Historie",
      "Reaktionen im Raum",
      "Chaos triggern",
    ]);
    expect(
      [...document.querySelectorAll(".room-detail-subheading")].map((el) => el.textContent),
    ).toContain("Prompt-Hinweise");
    // Perception hints or summary are always shown
    const emptyOrHints = document.querySelector(".room-detail-empty") || document.querySelector(".room-history-list");
    expect(emptyOrHints).not.toBeNull();
    expect(document.querySelector(".room-history-item.reaction")?.textContent).toContain(
      "Kontext: nach Raumreiz co2 seit t119",
    );
  });

  it("submits the chaos trigger with auth and refreshes badge plus detail", async () => {
    const { document, window } = installDom();
    await setTestAuth(true);

    const requests: Array<{ url: string; method: string; headers: Headers; body: string }> = [];
    let detailReads = 0;
    globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input);
      const method = init?.method ?? "GET";
      const headers = new Headers(init?.headers);
      const body = typeof init?.body === "string" ? init.body : "";
      requests.push({ url, method, headers, body });

      if (url.includes("/api/control/chaos")) {
        return new Response(
          JSON.stringify({
            accepted: true,
            event_id: "evt-42",
            room_id: "buero-design-2",
            chaos_type: "AirConBroken",
          }),
          {
            status: 202,
            headers: { "Content-Type": "application/json" },
          },
        );
      }

      detailReads += 1;
      const detailPayload = detailReads >= 2 ? UPDATED_DETAIL : INITIAL_DETAIL;
      return new Response(JSON.stringify(detailPayload), {
        status: 200,
        headers: { "Content-Type": "application/json" },
      });
    }) as typeof fetch;

    const { renderFloorplan } = await loadFloorplanModule();
    renderFloorplan(ROOMS);

    document
      .querySelector('[data-room-id="buero-design-2"]')
      ?.dispatchEvent(createClickEvent());
    await flushUi();
    await flushUi();

    const form = document.querySelector('[data-trigger-kind="chaos"]');
    expect(form).toBeTruthy();
    form?.dispatchEvent(new window.Event("submit", { bubbles: true, cancelable: true }));
    await flushUi();
    await flushUi();
    await flushUi();

    const chaosRequest = requests.find((request) => request.url.includes("/api/control/chaos"));
    expect(chaosRequest).toBeTruthy();
    expect(chaosRequest?.method).toBe("POST");
    // Kein Authorization-Header mehr — Auth laeuft ueber das httpOnly-Session-Cookie (#402).
    expect(chaosRequest?.headers.get("Authorization")).toBeNull();
    expect(JSON.parse(chaosRequest?.body ?? "{}")).toEqual({
      room_id: "buero-design-2",
      chaos_type: "AirConBroken",
    });

    expect(document.querySelector(".room-trigger-feedback")?.textContent).toContain("evt-42");
    expect(document.querySelector(".room-detail-chaos")?.textContent).toContain(
      "Klimaanlage defekt",
    );
    expect(
      document.querySelector('[data-room-id="buero-design-2"] .chaos-badge')?.textContent,
    ).toBe("AirConBroken");
  });

  it("submits a room stimulus trigger and refreshes the detail drawer", async () => {
    const { document, window } = installDom();
    await setTestAuth(true);

    const requests: Array<{ url: string; method: string; headers: Headers; body: string }> = [];
    let detailReads = 0;
    globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input);
      const method = init?.method ?? "GET";
      const headers = new Headers(init?.headers);
      const body = typeof init?.body === "string" ? init.body : "";
      requests.push({ url, method, headers, body });

      if (url.includes("/api/control/stimulus")) {
        return new Response(
          JSON.stringify({
            accepted: true,
            event_id: "stimulus-42",
            room_id: "buero-design-2",
            stimulus_type: "temperature",
            delta: 4,
          }),
          {
            status: 202,
            headers: { "Content-Type": "application/json" },
          },
        );
      }

      detailReads += 1;
      const detailPayload = detailReads >= 2 ? UPDATED_DETAIL : INITIAL_DETAIL;
      return new Response(JSON.stringify(detailPayload), {
        status: 200,
        headers: { "Content-Type": "application/json" },
      });
    }) as typeof fetch;

    const { renderFloorplan } = await loadFloorplanModule();
    renderFloorplan(ROOMS);

    document
      .querySelector('[data-room-id="buero-design-2"]')
      ?.dispatchEvent(createClickEvent());
    await flushUi();
    await flushUi();

    const deltaInput = document.querySelector(
      '[data-trigger-kind="stimulus"] input[type="number"]',
    ) as HTMLInputElement | null;
    expect(deltaInput).toBeTruthy();
    if (deltaInput) {
      deltaInput.value = "4";
      deltaInput.dispatchEvent(new window.Event("input", { bubbles: true }));
    }

    const form = document.querySelector('[data-trigger-kind="stimulus"]');
    expect(form).toBeTruthy();
    form?.dispatchEvent(new window.Event("submit", { bubbles: true, cancelable: true }));
    await flushUi();
    await flushUi();
    await flushUi();

    const stimulusRequest = requests.find((request) => request.url.includes("/api/control/stimulus"));
    expect(stimulusRequest).toBeTruthy();
    // Kein Authorization-Header mehr — Auth laeuft ueber das httpOnly-Session-Cookie (#402).
    expect(stimulusRequest?.headers.get("Authorization")).toBeNull();
    expect(JSON.parse(stimulusRequest?.body ?? "{}")).toEqual({
      room_id: "buero-design-2",
      stimulus_type: "temperature",
      delta: 4,
    });
    expect(document.querySelector(".room-trigger-feedback")?.textContent).toContain("stimulus-42");
    expect(document.querySelector(".room-history-item.reaction")?.textContent).toContain(
      "Mir ist zu warm hier.",
    );
  });
});
