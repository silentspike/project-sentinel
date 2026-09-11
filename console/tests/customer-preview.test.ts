import { describe, expect, it } from "vitest";
import { previewBroker, previewHtml, type PreviewInventory, type PreviewFile } from "../src/customer/preview";

const inventory: PreviewInventory = {
  project_id: "project-one", delivery: { id: "delivery-one", generation: 1, digest: "a".repeat(64) },
  release: { id: "release-one", generation: 2, digest: "b".repeat(64) },
  manifest_digest: "c".repeat(64), artifacts: [{ artifact_id: "site-source_tree-0", digest: "d".repeat(64), media_type: "application/json" }],
};
const file: PreviewFile = { ...inventory, artifact_id: "site-source_tree-0", path: "index.html", encoding: "base64", size_bytes: 4, content: "PHAvPg==" };

describe("customer preview", () => {
  it("decodes only the exact selected artifact and delivery", () => {
    expect(previewHtml(file, inventory, file.artifact_id, file.path)).toBe("<p/>");
    for (const change of [
      { project_id: "foreign" }, { delivery: { ...file.delivery, generation: 3 } },
      { release: { ...file.release, digest: "e".repeat(64) } }, { manifest_digest: "f".repeat(64) },
      { artifact_id: "other" }, { path: "other.html" }, { encoding: "raw" },
      { size_bytes: 5 }, { size_bytes: 0 }, { size_bytes: 1048577 },
      { content: "not base64" }, { content: "/w==", size_bytes: 1 },
    ]) expect(() => previewHtml({ ...file, ...change }, inventory, file.artifact_id, file.path)).toThrow();
  });
  it("creates only a fixed broker document with a constrained channel", () => {
    expect(() => previewBroker('</script>')).toThrow();
    const html = previewBroker("a".repeat(32));
    expect(html).toContain("event.source!==parent");
    expect(html).toContain("frame-src blob:");
    expect(html).toContain("script-src 'none'");
    expect(html).not.toContain("allow-same-origin");
    expect(html).not.toContain("innerHTML");
  });
});
