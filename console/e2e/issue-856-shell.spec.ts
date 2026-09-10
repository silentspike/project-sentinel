import { expect, test } from "@playwright/test";

test.beforeEach(async ({ page }) => {
  await page.route("**/api/**", async route => {
    const path = new URL(route.request().url()).pathname;
    await route.fulfill({ json: path === "/api/auth/status" ? { authenticated: true } : {} });
  });
});

for (const width of [768, 1024, 1440]) {
  test(`workspace navigation stays reachable at ${width}px`, async ({ page }, testInfo) => {
    await page.setViewportSize({ width, height: 900 });
    await page.goto("/");
    const toolbar = page.getByTestId("tiling-toolbar");
    await expect(toolbar).toBeVisible();
    await expect(toolbar.getByRole("button")).toHaveCount(18);
    const geometry = await toolbar.evaluate(element => ({
      viewport: window.innerWidth,
      document: document.documentElement.scrollWidth,
      buttons: [...element.querySelectorAll("button")].map(button => {
        const box = button.getBoundingClientRect();
        return { left: box.left, right: box.right, width: box.width, height: box.height };
      }),
    }));
    expect(geometry.document).toBeLessThanOrEqual(geometry.viewport);
    for (const button of geometry.buttons) {
      expect(button.left).toBeGreaterThanOrEqual(0);
      expect(button.right).toBeLessThanOrEqual(geometry.viewport);
      expect(button.width).toBeGreaterThan(0);
      expect(button.height).toBeGreaterThanOrEqual(32);
    }
    await page.getByTestId("open-agent-deep").focus();
    await page.keyboard.press("Enter");
    await expect(page.getByTestId("close-agent-deep")).toBeVisible();
    const tiles = page.locator('[data-testid^="tile-"]');
    await expect(tiles).toHaveCount(4);
    for (const tile of await tiles.all()) {
      await expect.poll(async () => (await tile.boundingBox())?.width ?? 0).toBeGreaterThanOrEqual(320);
      await expect.poll(async () => (await tile.boundingBox())?.height ?? 0).toBeGreaterThanOrEqual(220);
    }
    await page.getByTestId("close-metrics").focus();
    await expect(page.getByTestId("close-metrics")).toBeInViewport();
    expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(width);
    await page.getByTestId("tiling-root").evaluate(element => { element.scrollLeft = 0; });
    await page.screenshot({ path: testInfo.outputPath("workspace.png") });
  });
}

test("split controls support keyboard resizing and pointer drag after workspace scrolling", async ({ page }) => {
  await page.setViewportSize({ width: 1024, height: 900 });
  await page.goto("/");
  await page.getByTestId("open-agent-deep").click();
  const separator = page.getByTestId("tiling-root").locator(':scope > div > [role="separator"]');
  await expect(separator).toHaveAttribute("aria-orientation", "vertical");
  await separator.focus();
  await page.keyboard.press("ArrowRight");
  await expect(separator).toHaveAttribute("aria-valuenow", "39");
  await page.keyboard.press("Home");
  await expect(separator).toHaveAttribute("aria-valuenow", "10");
  await page.keyboard.press("ArrowLeft");
  await expect(separator).toHaveAttribute("aria-valuenow", "10");
  await page.keyboard.press("End");
  await expect(separator).toHaveAttribute("aria-valuenow", "90");

  const workspace = page.getByTestId("tiling-root");
  await workspace.evaluate(element => { element.scrollLeft = 200; });
  await expect.poll(() => workspace.evaluate(element => element.scrollLeft)).toBe(200);
  const splitId = (await separator.getAttribute("data-testid"))!.replace("gutter-", "split-");
  const split = await page.getByTestId(splitId).boundingBox();
  const gutter = await separator.boundingBox();
  expect(split).not.toBeNull();
  expect(gutter).not.toBeNull();
  const destination = gutter!.x + 50;
  const expected = Math.round(Math.max(0.1, Math.min(0.9, (destination - split!.x) / split!.width)) * 100);
  await page.mouse.move(gutter!.x + gutter!.width / 2, gutter!.y + 40);
  await page.mouse.down();
  await page.mouse.move(destination, gutter!.y + 40);
  await page.mouse.up();
  await expect(separator).toHaveAttribute("aria-valuenow", String(expected));
  await page.getByTestId("close-agent-deep").click();
  await expect(page.locator('[data-testid^="tile-"]')).toHaveCount(3);
  expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(1024);
});

test("vertical splits retain usable height and support keyboard resizing", async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 600 });
  await page.goto("/");
  await page.getByTestId("split-col-agents").click();
  await page.getByTestId("split-col-agents").click();
  const separator = page.locator('[role="separator"][aria-orientation="horizontal"]').first();
  await separator.focus();
  await page.keyboard.press("ArrowDown");
  await expect(separator).toHaveAttribute("aria-valuenow", "55");
  await page.keyboard.press("ArrowUp");
  await expect(separator).toHaveAttribute("aria-valuenow", "50");
  await expect(page.locator('[data-testid^="tile-"]')).toHaveCount(5);
  for (const tile of await page.locator('[data-testid^="tile-"]').all()) {
    await expect.poll(async () => (await tile.boundingBox())?.height ?? 0).toBeGreaterThanOrEqual(220);
  }
  const workspace = page.getByTestId("tiling-root");
  expect(await workspace.evaluate(element => element.scrollHeight > element.clientHeight)).toBe(true);
  expect(await page.evaluate(() => document.documentElement.scrollHeight)).toBeLessThanOrEqual(600);
});
