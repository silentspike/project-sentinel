import { describe, it, expect, vi, afterEach } from "vitest";
import { render, fireEvent, waitFor } from "@solidjs/testing-library";
import { GaiaConsoleView } from "../src/views/GaiaConsoleView";
import type { GaiaAlert, GaiaSessionIndexEntry, GaiaSessionRun } from "../src/api";

const ALERTS: GaiaAlert[] = [
  {
    alert_id: "gaia-alert-1",
    source_event_id: "event-1",
    tick: 42,
    timestamp_ms: 1_800_000_000_000,
    trigger: "unresolved_escalation",
    severity: "warning",
    target: "system",
    summary: "Projection lag",
    recommendation: "Inspect projection",
    unresolved_keys: ["projection:lag"],
  },
];

const SESSIONS: GaiaSessionIndexEntry[] = [
  {
    gaia_session_id: "gaia-deep-test",
    claude_session_id: "claude-test",
    kind: "deep",
    status: "succeeded",
    stream_path: "/tmp/stream.jsonl",
    started_at_ms: 1_800_000_000_100,
    finished_at_ms: 1_800_000_000_200,
    exit_code: 0,
    usage: {
      input_tokens: 2,
      output_tokens: 3,
      cache_read_input_tokens: 0,
      cache_creation_input_tokens: 0,
      total_cost_usd: 0.0005,
    },
  },
];

const RUN: GaiaSessionRun = {
  entry: {
    ...SESSIONS[0],
    gaia_session_id: "gaia-deep-new",
    claude_session_id: "resume-test",
  },
  session_dir: "/tmp/gaia-deep-new",
  prompt_path: "/tmp/gaia-deep-new/prompt.txt",
  stderr_path: "/tmp/gaia-deep-new/stderr.log",
};

const SETUP_RUN: GaiaSessionRun = {
  ...RUN,
  entry: {
    ...RUN.entry,
    gaia_session_id: "gaia-setup-new",
    kind: "setup_interview",
  },
};

const posted: { url: string; body: string }[] = [];

function jsonOk(value: unknown) {
  return { ok: true, statusText: "OK", json: async () => value };
}

function textOk(value: string) {
  return { ok: true, statusText: "OK", text: async () => value };
}

function stubFetch() {
  posted.length = 0;
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: string, init?: RequestInit) => {
      const u = String(url);
      if (u.includes("/api/gaia/sessions/") && u.endsWith("/stream")) {
        return textOk('{"type":"message","result":"completed"}\n');
      }
      if (init?.method === "POST" && u.includes("/api/gaia/deep")) {
        posted.push({ url: u, body: String(init.body) });
        return jsonOk(RUN);
      }
      if (init?.method === "POST" && u.includes("/api/gaia/setup-interview")) {
        posted.push({ url: u, body: String(init.body) });
        return jsonOk(SETUP_RUN);
      }
      if (u.includes("/api/gaia/alerts")) return jsonOk({ alerts: ALERTS, count: ALERTS.length, source: "/tmp/alerts.jsonl" });
      if (u.includes("/api/gaia/sessions")) return jsonOk({ sessions: SESSIONS, count: SESSIONS.length, source: "/tmp/index.jsonl" });
      return { ok: false, statusText: "missing", json: async () => ({ error: "missing" }) };
    }),
  );
}

afterEach(() => {
  vi.unstubAllGlobals();
  posted.length = 0;
});

describe("GaiaConsoleView (#442)", () => {
  it("renders alerts, sessions, and selected raw stream", async () => {
    stubFetch();
    const { getByTestId, getByText } = render(GaiaConsoleView);

    await waitFor(() => expect(getByText("Projection lag")).toBeTruthy());
    expect(getByTestId("gaia-alert-count").textContent).toContain("Alerts 1");
    expect(getByTestId("gaia-session-count").textContent).toContain("Sessions 1");
    expect(getByTestId("gaia-sessions").textContent).toContain("gaia-deep-test");

    fireEvent.click(getByTestId("gaia-session-row"));
    await waitFor(() => expect(getByTestId("gaia-stream").textContent).toContain("completed"));
  });

  it("starts a deep session with prompt and resume id", async () => {
    stubFetch();
    const { getByTestId } = render(GaiaConsoleView);
    await waitFor(() => expect(getByTestId("gaia-start")).toBeTruthy());

    const prompt = getByTestId("gaia-prompt") as HTMLTextAreaElement;
    prompt.value = "inspect open tasks";
    fireEvent.input(prompt);
    const resume = getByTestId("gaia-resume") as HTMLInputElement;
    resume.value = "resume-test";
    fireEvent.input(resume);
    fireEvent.click(getByTestId("gaia-start"));

    await waitFor(() => expect(posted.some((call) => call.url.includes("/api/gaia/deep"))).toBe(true));
    expect(posted[0].body).toContain("inspect open tasks");
    expect(posted[0].body).toContain("resume-test");
    await waitFor(() => expect(getByTestId("gaia-stream").textContent).toContain("completed"));
  });

  it("starts a setup interview session from the setup mode", async () => {
    stubFetch();
    const { getByTestId } = render(GaiaConsoleView);
    await waitFor(() => expect(getByTestId("gaia-mode-setup")).toBeTruthy());

    fireEvent.click(getByTestId("gaia-mode-setup"));
    const prompt = getByTestId("gaia-prompt") as HTMLTextAreaElement;
    prompt.value = "prepare onboarding";
    fireEvent.input(prompt);
    fireEvent.click(getByTestId("gaia-start"));

    await waitFor(() => expect(posted.some((call) => call.url.includes("/api/gaia/setup-interview"))).toBe(true));
  });
});
