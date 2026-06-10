import { afterEach, describe, expect, it, vi } from "vitest";
import { login, type LoginResult } from "../src/auth";

// #474: login() maps the backend response to a distinct UX outcome so the operator can tell
// "wrong key" (401) from "rate-limited" (429). auth.ts is JSX-free -> safe to import in vitest.

function mockFetch(status: number, body: unknown) {
  vi.stubGlobal(
    "fetch",
    vi.fn(async () => ({
      ok: status >= 200 && status < 300,
      status,
      json: async () => body,
    })),
  );
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("login() outcome mapping (#474)", () => {
  it("200 + authenticated:true -> ok", async () => {
    mockFetch(200, { authenticated: true });
    expect<LoginResult>(await login("good")).toBe("ok");
  });

  it("200 + authenticated:false -> invalid", async () => {
    mockFetch(200, { authenticated: false });
    expect(await login("weird")).toBe("invalid");
  });

  it("401 -> invalid (wrong key)", async () => {
    mockFetch(401, { authenticated: false });
    expect(await login("bad")).toBe("invalid");
  });

  it("429 -> rate-limited (distinct from invalid)", async () => {
    mockFetch(429, { error: "too many", retry_after_secs: 300, authenticated: false });
    expect(await login("bad")).toBe("rate-limited");
  });

  it("network throw -> invalid (no crash)", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => {
        throw new Error("network down");
      }),
    );
    expect(await login("good")).toBe("invalid");
  });
});
