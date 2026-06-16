import { describe, it, expect, vi, afterEach } from "vitest";
import { render, fireEvent, waitFor } from "@solidjs/testing-library";
import type { AgentConfig } from "../src/api";
import { AgentEditorView } from "../src/views/AgentEditorView";

const agent: AgentConfig = {
  identity: {
    id: 1,
    name: "Thomas",
    role: "CEO",
    department: "Leitung",
    shift_set: 1,
    kpis: ["revenue"],
    reports_to: null,
    direct_reports: [],
  },
  personality: {
    openness: 0.5,
    conscientiousness: 0.6,
    extraversion: 0.4,
    agreeableness: 0.5,
    neuroticism: 0.3,
    caffeine_tolerance: 0.7,
    morning_person: true,
  },
  preferences: { favorite_room: "buero-ceo", coffee_preference: "espresso", lunch_time: "12:30" },
  background: { bio: "Founder.", quirks: ["paces"] },
  runtime: { nano_runtime: null },
  capabilities: { tools: [], sandbox_allowed_paths: [] },
};

afterEach(() => vi.unstubAllGlobals());

describe("AgentEditorView (#422)", () => {
  it("renders the editor shell with a selection control", () => {
    const { getByTestId } = render(AgentEditorView);
    expect(getByTestId("view-agent-editor")).toBeTruthy();
    expect(getByTestId("ae-select")).toBeTruthy();
  });

  it("loads agents, shows the form on select, and gates save on dirty+valid", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => ({ ok: true, json: async () => [agent] })),
    );
    const { getByTestId, findByTestId } = render(AgentEditorView);
    const select = getByTestId("ae-select") as HTMLSelectElement;
    await waitFor(() => expect(select.querySelectorAll("option").length).toBeGreaterThan(1));

    select.value = "1";
    fireEvent.change(select);

    const save = (await findByTestId("ae-save")) as HTMLButtonElement;
    expect(save.disabled).toBe(true); // loaded but not dirty -> disabled
    // nano_runtime is read-only.
    const nano = getByTestId("ae-nano-runtime") as HTMLInputElement;
    expect(nano.readOnly).toBe(true);

    // Edit a field -> dirty -> save enabled (values still valid).
    const name = getByTestId("ae-name") as HTMLInputElement;
    fireEvent.input(name, { target: { value: "Thomas R." } });
    await waitFor(() => expect(save.disabled).toBe(false));
  });
});
