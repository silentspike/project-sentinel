import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, waitFor } from "@solidjs/testing-library";
import App from "../src/App";
import { authStatus, login, logout, type AuthOutcomeBinding } from "../src/auth";
import { connectTransport } from "../src/stores/console";

vi.mock("../src/tiling/TilingLayout", () => ({ Tiling: () => null }));
vi.mock("../src/auth", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../src/auth")>();
  return { ...actual, authStatus: vi.fn(actual.authStatus) };
});
vi.mock("../src/stores/console", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../src/stores/console")>()),
  connectTransport: vi.fn(),
}));

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: Error) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}

function response(authenticated: unknown, status = 200): Response {
  return new Response(JSON.stringify({ authenticated }), { status, headers: { "content-type": "application/json" } });
}

function binding(authenticated = false): AuthOutcomeBinding {
  return { generation: 0, authenticated };
}

beforeEach(() => {
  vi.stubGlobal("matchMedia", () => ({ matches: false, addEventListener: vi.fn(), removeEventListener: vi.fn() }));
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  vi.clearAllMocks();
});

describe("generation-bound operator authentication", () => {
  it("keeps the real App shell authenticated when initial status false resolves after login success", async () => {
    const initial = deferred<Response>();
    const fetch = vi.fn((path: string) => path === "/api/auth/status" ? initial.promise : Promise.resolve(response(true)));
    vi.stubGlobal("fetch", fetch);
    const view = render(App);
    const restoring = vi.mocked(authStatus).mock.results[0].value as Promise<boolean>;
    fireEvent.input(view.getByTestId("login-key"), { target: { value: "operator-test-key" } });
    fireEvent.click(view.getByTestId("login-submit"));
    await waitFor(() => expect(view.queryByTestId("shell")).not.toBeNull());

    initial.resolve(response(false));
    await restoring;
    await Promise.resolve();
    expect(view.queryByTestId("shell")).not.toBeNull();
    expect(view.queryByTestId("login")).toBeNull();
    expect(connectTransport).toHaveBeenCalledTimes(1);
    expect(fetch).toHaveBeenCalledWith("/api/auth/login", expect.objectContaining({ method: "POST", credentials: "include" }));
  });

  it.each([true, false])("applies normal boot status %s without another auth operation", async (authenticated) => {
    const initial = deferred<Response>();
    vi.stubGlobal("fetch", vi.fn(() => initial.promise));
    const view = render(App);
    const restoring = vi.mocked(authStatus).mock.results[0].value as Promise<boolean>;
    expect(view.queryByTestId("login")).not.toBeNull();
    initial.resolve(response(authenticated));
    await restoring;
    await waitFor(() => expect(Boolean(view.queryByTestId("shell"))).toBe(authenticated));
    if (authenticated) expect(connectTransport).toHaveBeenCalledTimes(1);
    else expect(connectTransport).not.toHaveBeenCalled();
  });

  it("does not open the App shell for a 200 login response with authenticated false", async () => {
    const initial = deferred<Response>();
    vi.stubGlobal("fetch", vi.fn((path: string) => path === "/api/auth/status" ? initial.promise : Promise.resolve(response(false))));
    const view = render(App);
    const restoring = vi.mocked(authStatus).mock.results[0].value as Promise<boolean>;
    fireEvent.click(view.getByTestId("login-submit"));
    await waitFor(() => expect(view.queryByTestId("login-error")).not.toBeNull());
    initial.resolve(response(true));
    await restoring;
    await Promise.resolve();
    expect(view.queryByTestId("shell")).toBeNull();
    expect(connectTransport).not.toHaveBeenCalled();
  });

  it("returns the current login outcome instead of a delayed initial false", async () => {
    const initial = deferred<Response>();
    vi.stubGlobal("fetch", vi.fn((path: string) => path === "/api/auth/status" ? initial.promise : Promise.resolve(response(true))));
    const state = binding();
    const restore = authStatus(state);
    expect(await login("operator-test-key", state)).toBe("ok");
    initial.resolve(response(false));
    expect(await restore).toBe(true);
    expect(state.authenticated).toBe(true);
  });

  it("keeps logout false when an older initial status later resolves true", async () => {
    const initial = deferred<Response>();
    const fetch = vi.fn((path: string) => path === "/api/auth/status" ? initial.promise : Promise.resolve(response(false)));
    vi.stubGlobal("fetch", fetch);
    const state = binding(true);
    const restore = authStatus(state);
    await logout(state);
    initial.resolve(response(true));
    expect(await restore).toBe(false);
    expect(state.authenticated).toBe(false);
    expect(fetch).toHaveBeenCalledWith("/api/auth/logout", { method: "POST", credentials: "include" });
  });

  it("does not let a late successful login override a newer logout", async () => {
    const pending = deferred<Response>();
    vi.stubGlobal("fetch", vi.fn((path: string) => path === "/api/auth/login" ? pending.promise : Promise.resolve(response(false))));
    const state = binding();
    const loggingIn = login("operator-test-key", state);
    await logout(state);
    pending.resolve(response(true));
    expect(await loggingIn).toBe("invalid");
    expect(state.authenticated).toBe(false);
  });

  it("invalidates an old positive even if the logout request fails", async () => {
    const initial = deferred<Response>();
    vi.stubGlobal("fetch", vi.fn((path: string) => path === "/api/auth/status" ? initial.promise : Promise.reject(new Error("offline"))));
    const state = binding(true);
    const restore = authStatus(state);
    await logout(state);
    initial.resolve(response(true));
    expect(await restore).toBe(false);
    expect(state.authenticated).toBe(false);
  });

  it.each([401, 429])("preserves failed login outcome %s while discarding an older positive", async (status) => {
    const initial = deferred<Response>();
    vi.stubGlobal("fetch", vi.fn((path: string) => path === "/api/auth/status" ? initial.promise : Promise.resolve(response(false, status))));
    const state = binding();
    const restore = authStatus(state);
    expect(await login("invalid-test-key", state)).toBe(status === 429 ? "rate-limited" : "invalid");
    initial.resolve(response(true));
    expect(await restore).toBe(false);
  });

  it("binds generations per App session rather than sharing authentication between instances", async () => {
    const initial = deferred<Response>();
    vi.stubGlobal("fetch", vi.fn((path: string) => path === "/api/auth/status" ? initial.promise : Promise.resolve(response(true))));
    const first = binding();
    const second = binding();
    const restore = authStatus(second);
    await login("operator-test-key", first);
    initial.resolve(response(false));
    expect(await restore).toBe(false);
    expect(first.authenticated).toBe(true);
    expect(second.authenticated).toBe(false);
  });

  it("fails closed for a non-boolean authentication DTO", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => response("true")));
    expect(await authStatus(binding())).toBe(false);
    expect(await login("operator-test-key", binding())).toBe("invalid");
  });

  it("does not publish a stale failed login while a newer login is pending", async () => {
    const first = deferred<Response>();
    const second = deferred<Response>();
    const initial = deferred<Response>();
    let calls = 0;
    vi.stubGlobal("fetch", vi.fn((path: string) => {
      if (path === "/api/auth/status") return initial.promise;
      return ++calls === 1 ? first.promise : second.promise;
    }));
    const view = render(App);
    fireEvent.click(view.getByTestId("login-submit"));
    fireEvent.click(view.getByTestId("login-submit"));
    first.resolve(response(false, 401));
    await first.promise;
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(view.queryByTestId("login-error")).toBeNull();
    second.resolve(response(true));
    await waitFor(() => expect(view.queryByTestId("shell")).not.toBeNull());
    initial.resolve(response(false));
  });

  it("does not start transport after an unmounted App receives an old positive", async () => {
    const initial = deferred<Response>();
    vi.stubGlobal("fetch", vi.fn(() => initial.promise));
    const view = render(App);
    const restoring = vi.mocked(authStatus).mock.results[0].value as Promise<boolean>;
    view.unmount();
    initial.resolve(response(true));
    await restoring;
    await Promise.resolve();
    expect(connectTransport).not.toHaveBeenCalled();
  });
});
