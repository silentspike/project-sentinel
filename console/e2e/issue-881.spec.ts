import { expect, test } from "@playwright/test";

for (const width of [390, 768, 1440]) {
  test(`delivery authority fields fit a ${width}px viewport`, async ({ page }) => {
    await page.setViewportSize({ width, height: 1000 });
    await page.route("**/api/**", async route => {
      const path = new URL(route.request().url()).pathname;
      if (path === "/api/auth/status") {
        await route.fulfill({ json: { authenticated: true } });
      } else if (path === "/api/v1/delivery/lineage") {
        await route.fulfill({ json: {
          schemaVersion: 1, serverRedacted: true, projectLabel: "Project",
          revision: 1, adapterReady: true, authorityGeneration: 1, readAtMs: 1,
          nodes: [{ id: "record-1", stage: "release", label: "Approved customer delivery",
            state: "completed", digest: "a".repeat(64), generation: 1,
            actorRole: "release_manager", costMinor: "125", currency: "USD" }],
          edges: [], blockers: [],
        } });
      } else {
        await route.fulfill({ json: {} });
      }
    });
    await page.goto("/?tenant_id=fixture&project_id=fixture");
    await page.getByTestId("open-delivery").click();
    await expect(page.getByTestId("delivery-lineage-node")).toHaveCount(1);
    await expect(page.getByTestId("delivery-invalid")).toHaveCount(0);
    await expect(page.getByTestId("delivery-authority")).toHaveText("release_manager");
    await expect(page.getByTestId("delivery-cost")).toHaveText("USD 1.25");
    const geometry = await page.getByTestId("delivery-lineage-node").evaluate(element => {
      const parent = element.getBoundingClientRect();
      return {
        fields: [...element.children].map(child => {
          const rect = child.getBoundingClientRect();
          return { left: rect.left, right: rect.right, overflow: child.scrollWidth > child.clientWidth + 1 };
        }),
        left: parent.left, right: parent.right, viewport: window.innerWidth,
      };
    });
    expect(geometry.fields).toHaveLength(5);
    expect(geometry.right).toBeLessThanOrEqual(geometry.viewport);
    for (const field of geometry.fields) {
      expect(field.left).toBeGreaterThanOrEqual(geometry.left);
      expect(field.right).toBeLessThanOrEqual(geometry.right + 1);
      expect(field.overflow).toBe(false);
    }
    await page.screenshot({ path: test.info().outputPath(`delivery-${width}.png`) });
  });
}
