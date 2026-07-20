import { expect, test, type Page } from "@playwright/test";

const agent = {
  identity: {
    id: 2,
    name: "Lisa",
    role: "Designer",
    department: "Design",
    tier: 2,
    shift_set: 1,
    kpis: [],
    reports_to: "Thomas",
    direct_reports: [],
  },
  personality: {
    openness: 0.5,
    conscientiousness: 0.5,
    extraversion: 0.5,
    agreeableness: 0.5,
    neuroticism: 0.3,
    caffeine_tolerance: 0.5,
    morning_person: true,
  },
  preferences: { favorite_room: "Design", coffee_preference: "espresso", lunch_time: "12:00" },
  background: { bio: "Designer", quirks: ["pixel precise"] },
  runtime: { nano_runtime: "local-loop" },
  capabilities: { tools: [], sandbox_allowed_paths: [] },
};

async function mockApi(page: Page, agents: unknown[] = [agent], failAgents = false) {
  let savedBody: unknown = null;
  let servedAgents = agents;
  await page.route("**/api/**", async (route) => {
    const request = route.request();
    const path = new URL(request.url()).pathname;
    if (path === "/api/auth/status") {
      await route.fulfill({ json: { authenticated: true } });
    } else if (path === "/api/cert-hash") {
      await route.fulfill({ json: { algorithm: "sha-256", hash: null } });
    } else if (path === "/api/config/agents" && request.method() === "GET") {
      await route.fulfill(
        failAgents
          ? { status: 503, json: { error: "projection offline" } }
          : { json: servedAgents },
      );
    } else if (/^\/api\/config\/agents\/\d+$/.test(path) && request.method() === "PUT") {
      savedBody = request.postDataJSON();
      const updated = savedBody as typeof agent;
      servedAgents = servedAgents.map((candidate) =>
        (candidate as typeof agent).identity?.id === updated.identity.id ? updated : candidate,
      );
      await route.fulfill({ json: { ok: true } });
    } else {
      await route.fulfill({ json: {} });
    }
  });
  return () => savedBody;
}

function captureBrowserFailures(page: Page, allowed: RegExp[] = []): () => void {
  const failures: string[] = [];
  page.on("pageerror", (error) => failures.push(`pageerror: ${error.message}`));
  page.on("console", (message) => {
    if (message.type() === "error") failures.push(`console: ${message.text()}`);
  });
  page.on("requestfailed", (request) => {
    failures.push(`requestfailed: ${request.method()} ${new URL(request.url()).pathname}`);
  });
  return () => expect(failures.filter((failure) => !allowed.some((pattern) => pattern.test(failure)))).toEqual([]);
}

test("desktop org chart opens the editor and persists the selected hierarchy tier", async ({ page }) => {
  await page.setViewportSize({ width: 1920, height: 1080 });
  const assertNoBrowserFailures = captureBrowserFailures(page);
  const savedBody = await mockApi(page);
  await page.goto("/");
  await expect(page.getByTestId("shell")).toBeVisible();
  await page.getByTestId("close-metrics").click();
  await page.getByTestId("close-floorplan").click();
  await page.getByTestId("open-org-chart").click();

  const agentButton = page.getByRole("button", { name: /Lisa.*hierarchy tier 2/ });
  await expect(agentButton).toBeVisible();
  await agentButton.focus();
  await expect(agentButton).toBeFocused();
  await agentButton.press("Enter");

  const editor = page.getByTestId("view-agent-editor");
  await expect(editor).toBeVisible();
  await expect(editor.getByTestId("ae-select")).toHaveValue("2");
  await editor.getByTestId("ae-hierarchy-tier").selectOption("3");
  await expect(editor.getByTestId("ae-nano-runtime")).toHaveValue("local-loop");
  await editor.getByTestId("ae-save").click();
  await expect.poll(savedBody).not.toBeNull();
  expect(savedBody()).toMatchObject({ identity: { id: 2, tier: 3 }, runtime: { nano_runtime: "local-loop" } });
  await expect(page.getByTestId("org-tier")).toContainText("hierarchy tier: 3", { timeout: 12_000 });
  expect(await editor.evaluate((element) => element.scrollWidth <= element.clientWidth)).toBe(true);
  await editor
    .getByTestId("ae-hierarchy-tier")
    .evaluate((element) => element.scrollIntoView({ block: "center" }));
  await expect(editor.getByTestId("ae-hierarchy-tier")).toHaveValue("3");

  await page.screenshot({
    path: "evidence/issue-395-live/desktop-org-chart-agent-editor.png",
    fullPage: true,
  });
  assertNoBrowserFailures();
});

test("narrow org chart remains within the viewport", async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  const assertNoBrowserFailures = captureBrowserFailures(page);
  await mockApi(page);
  await page.goto("/");
  await page.getByTestId("tab-org-chart").click();
  await expect(page.getByTestId("org-agent-node")).toBeVisible();
  const hasDocumentOverflow = await page.evaluate(
    () => document.documentElement.scrollWidth > document.documentElement.clientWidth,
  );
  expect(hasDocumentOverflow).toBe(false);
  await page.screenshot({
    path: "evidence/issue-395-live/mobile-org-chart.png",
    fullPage: true,
  });
  assertNoBrowserFailures();
});

test("org chart exposes distinct empty and error states", async ({ page }) => {
  await page.setViewportSize({ width: 1024, height: 768 });
  const assertNoBrowserFailures = captureBrowserFailures(page, [/console: Failed to load resource:.*503/]);
  await mockApi(page, []);
  await page.goto("/");
  await page.getByTestId("open-org-chart").click();
  await expect(page.getByTestId("org-empty")).toBeVisible();

  await page.unrouteAll({ behavior: "wait" });
  await mockApi(page, [], true);
  await page.reload();
  await page.getByTestId("open-org-chart").click();
  await expect(page.getByTestId("org-error")).toContainText("projection offline");
  await page.screenshot({
    path: "evidence/issue-395-live/org-chart-error.png",
    fullPage: true,
  });
  assertNoBrowserFailures();
});
