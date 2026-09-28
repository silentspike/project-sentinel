import { createComponent, createSignal } from "solid-js";
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, waitFor } from "@solidjs/testing-library";
import { CustomerPreview } from "../src/customer/CustomerPreview";
import { customerFetch, type CustomerDelivery } from "../src/customer/api";
import type { PreviewInventory } from "../src/customer/preview";

vi.mock("../src/customer/api", async importOriginal => ({
  ...await importOriginal<typeof import("../src/customer/api")>(), customerFetch: vi.fn(),
}));

const delivered: CustomerDelivery = {
  delivery: { id: "delivery-one", generation: 1, digest: "a".repeat(64) },
  release: { id: "release-one", generation: 1, digest: "b".repeat(64) },
  state: "delivered", release_state: "active", issued_at_ms: Date.now(),
  expires_at_ms: Date.now() + 60_000, preview_digest: "c".repeat(64),
};
const inventory: PreviewInventory = { project_id: "project-one", delivery: delivered.delivery, release: delivered.release,
  manifest_digest: "d".repeat(64), artifacts: [{ artifact_id: "source_tree", digest: "e".repeat(64), media_type: "application/json" }] };
const encoded = (path: string, text: string) => ({ ...inventory, artifact_id: "source_tree", path, encoding: "base64",
  size_bytes: new TextEncoder().encode(text).length, content: btoa(text) });

afterEach(() => { cleanup(); vi.resetAllMocks(); });

async function opened() {
  const [delivery, setDelivery] = createSignal(structuredClone(delivered));
  const view = render(() => createComponent(CustomerPreview, { projectId: "project-one", get delivery() { return delivery(); }, close: () => {} }));
  await waitFor(() => expect((view.getByRole("button", { name: "Vorschau laden" }) as HTMLButtonElement).disabled).toBe(false));
  fireEvent.click(view.getByRole("button", { name: "Vorschau laden" }));
  const frame = await waitFor(() => view.getByTitle("Isolierte Lieferungsvorschau") as HTMLIFrameElement);
  const channel = frame.srcdoc.match(/const channel="([a-f0-9]+)"/)![1];
  const post = vi.spyOn(frame.contentWindow!, "postMessage");
  const message = (kind: string, hrefs?: unknown, source = frame.contentWindow, messageChannel = channel) => {
    window.dispatchEvent(new MessageEvent("message", { source, data: { kind, channel: messageChannel, hrefs } }));
  };
  return { view, frame, post, message, setDelivery };
}

function responses(css = "body{color:red}") {
  vi.mocked(customerFetch).mockImplementation(async (_path, body) => {
    const file = (body as { file?: { path: string } }).file;
    return (!file ? structuredClone(inventory) : encoded(file.path, file.path.endsWith(".css") ? css : "<p>Preview</p>")) as never;
  });
}

describe("customer preview bound assets", () => {
  it("fetches and relays local styles with exact authority, caching duplicates once", async () => {
    responses();
    const target = await opened();
    target.message("preview-stylesheets", ["style.css"], window);
    target.message("preview-stylesheets", ["style.css"], target.frame.contentWindow, "f".repeat(32));
    expect(vi.mocked(customerFetch)).toHaveBeenCalledTimes(2);
    target.message("preview-stylesheets", ["style.css", "style.css"]);
    await waitFor(() => expect(target.post).toHaveBeenCalledWith(expect.objectContaining({ kind: "stylesheets", styles: ["body{color:red}", "body{color:red}"] }), "*"));
    const calls = vi.mocked(customerFetch).mock.calls;
    expect(calls).toHaveLength(3);
    expect(calls[2][1]).toEqual({ project_id: "project-one", delivery: delivered.delivery, release: delivered.release,
      file: { artifact_id: "source_tree", path: "style.css" } });
    target.message("preview-stylesheets", ["style.css"]);
    expect(vi.mocked(customerFetch)).toHaveBeenCalledTimes(3);
  });
  it.each(["https://foreign.invalid/style.css", "../style.css", Array(17).fill("style.css")])("rejects invalid stylesheet requests without resource I/O: %s", async href => {
    responses();
    const target = await opened();
    target.message("preview-stylesheets", Array.isArray(href) ? href : [href]);
    await waitFor(() => expect(target.view.queryByTitle("Isolierte Lieferungsvorschau")).toBeNull());
    expect(vi.mocked(customerFetch)).toHaveBeenCalledTimes(2);
  });
  it("rejects a stylesheet response from another manifest", async () => {
    responses();
    const target = await opened();
    vi.mocked(customerFetch).mockResolvedValueOnce({ ...encoded("style.css", "body{}"), manifest_digest: "f".repeat(64) });
    target.message("preview-stylesheets", ["style.css"]);
    await waitFor(() => expect(target.view.getByRole("alert").textContent).toContain("preview_response_binding_invalid"));
    expect(target.post).not.toHaveBeenCalled();
    expect(target.view.queryByTitle("Isolierte Lieferungsvorschau")).toBeNull();
  });
  it("drops an in-flight stylesheet when the release is revoked", async () => {
    responses();
    const target = await opened();
    let resolve!: (value: unknown) => void;
    vi.mocked(customerFetch).mockImplementationOnce(() => new Promise(value => { resolve = value; }) as never);
    target.message("preview-stylesheets", ["style.css"]);
    target.setDelivery({ ...delivered, release_state: "revoked" });
    resolve(encoded("style.css", "body{}"));
    await waitFor(() => expect(target.view.queryByTitle("Isolierte Lieferungsvorschau")).toBeNull());
    expect(target.post).not.toHaveBeenCalled();
  });
  it("rejects aggregate stylesheet bytes above the bound", async () => {
    responses("a".repeat(600_000));
    const target = await opened();
    target.message("preview-stylesheets", ["one.css", "two.css"]);
    await waitFor(() => expect(target.view.getByRole("alert").textContent).toContain("preview_stylesheets_too_large"));
    expect(target.post).not.toHaveBeenCalled();
  });
});
