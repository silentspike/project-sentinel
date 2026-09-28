import { describe, expect, it } from "vitest";
import { previewBroker, previewHtml, previewStylesheetPath, type PreviewInventory, type PreviewFile } from "../src/customer/preview";

const inventory: PreviewInventory = {
  project_id: "project-one", delivery: { id: "delivery-one", generation: 1, digest: "a".repeat(64) },
  release: { id: "release-one", generation: 2, digest: "b".repeat(64) },
  manifest_digest: "c".repeat(64), artifacts: [{ artifact_id: "site-source_tree-0", digest: "d".repeat(64), media_type: "application/json" }],
};
const file: PreviewFile = { ...inventory, artifact_id: "site-source_tree-0", path: "index.html", encoding: "base64", size_bytes: 4, content: "PHAvPg==" };

describe("customer preview", () => {
  it("binds renewed inventory and assets to the exact access grant", () => {
    const access = { id: "preview-access", generation: 3, digest: "e".repeat(64) };
    const renewed = { ...inventory, preview_access: access };
    expect(previewHtml({ ...file, preview_access: access }, renewed, file.artifact_id, file.path)).toBe("<p/>");
    for (const preview_access of [undefined, null, { ...access, generation: 4 }, { ...access, digest: "f".repeat(64) }]) {
      expect(() => previewHtml({ ...file, preview_access }, renewed, file.artifact_id, file.path)).toThrow();
    }
    expect(() => previewHtml({ ...file, preview_access: access }, inventory, file.artifact_id, file.path)).toThrow();
  });
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
  it("resolves only local CSS paths without escaping the selected artifact", () => {
    expect(previewStylesheetPath("index.html", "style.css")).toBe("style.css");
    expect(previewStylesheetPath("pages/index.html", "../css/style.css")).toBe("css/style.css");
    expect(previewStylesheetPath("pages/index.html", "/style.css")).toBe("style.css");
    expect(previewStylesheetPath("index.html", "%73tyle.css")).toBe("style.css");
    for (const href of [null, [], "", "https://remote.invalid/style.css", "//remote.invalid/style.css", "file:///style.css",
      "data:text/css,body{}", "javascript:alert(1)", "../style.css", "%2e%2e/style.css", "%252e%252e/style.css", "\\style.css",
      "style.css?token=secret", "style.css#fragment", "style.css\n", "other.html", "%zz", "a".repeat(1025)]) {
      expect(() => previewStylesheetPath("index.html", href)).toThrow("preview_stylesheet_path_invalid");
    }
    for (const path of ["../index.html", "/index.html", "https://remote.invalid/index.html", "index.html?x=1"]) {
      expect(() => previewStylesheetPath(path, "style.css")).toThrow("preview_stylesheet_path_invalid");
    }
  });
});
