import { expect, test, type Page } from "@playwright/test";

const runtimeEnv = (
  globalThis as typeof globalThis & {
    process?: { env?: Record<string, string | undefined> };
  }
).process?.env;
const dashboardKey = runtimeEnv?.ISSUE395_DASHBOARD_KEY;
test.skip(!dashboardKey, "ISSUE395_DASHBOARD_KEY is required for the authorized live run");

async function login(page: Page): Promise<void> {
  await page.goto("/");
  const keyInput = page.getByTestId("login-key");
  if (await keyInput.isVisible()) {
    await keyInput.fill(dashboardKey!);
    await page.getByTestId("login-submit").click();
  }
  await expect(page.getByTestId("shell")).toBeVisible();
}

async function liveAgentTier(page: Page, agentId: number): Promise<number | undefined> {
  return page.evaluate(async (id) => {
    const response = await fetch("/api/config/agents", { credentials: "include" });
    const agents = await response.json();
    return agents.find((agent: { identity?: { id?: number } }) => agent.identity?.id === id)?.identity?.tier;
  }, agentId);
}

async function updateLiveAgentTier(page: Page, agentId: number, tier: number): Promise<void> {
  const status = await page.evaluate(async ({ id, nextTier }) => {
    const response = await fetch("/api/config/agents", { credentials: "include" });
    const agents = await response.json();
    const agent = agents.find((candidate: { identity?: { id?: number } }) => candidate.identity?.id === id);
    agent.identity.tier = nextTier;
    const update = await fetch(`/api/config/agents/${id}`, {
      method: "PUT",
      credentials: "include",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(agent),
    });
    return update.status;
  }, { id: agentId, nextTier: tier });
  expect(status).toBe(202);
  await expect.poll(() => liveAgentTier(page, agentId), { timeout: 20_000 }).toBe(tier);
}

function captureUnexpectedFailures(page: Page): () => void {
  const failures: string[] = [];
  const expectedTunnelLimitation = /WebTransport|QUIC|ERR_QUIC|\/wt\b|\/favicon\.ico\b|Failed to establish a connection to https:\/\/127\.0\.0\.1:18401\/(?:\?t=[0-9a-f-]+)?: net::ERR_CONNECTION_REFUSED|net::ERR_ABORTED https:\/\/127\.0\.0\.1:18401\/api\/metrics\/(?:ebpf|pipeline|tick|phases)\b/i;
  page.on("pageerror", (error) => failures.push(`pageerror: ${error.message}`));
  page.on("console", (message) => {
    const entry = `${message.text()} ${message.location().url}`;
    if (message.type() === "error" && !expectedTunnelLimitation.test(entry)) {
      failures.push(`console: ${entry}`);
    }
  });
  page.on("requestfailed", (request) => {
    const failure = `${request.failure()?.errorText ?? "failed"} ${request.url()}`;
    if (!expectedTunnelLimitation.test(failure)) failures.push(`requestfailed: ${failure}`);
  });
  return () => expect(failures).toEqual([]);
}

test("live desktop org chart edits and restores hierarchy tier", async ({ page }) => {
  await page.setViewportSize({ width: 1920, height: 1080 });
  const assertNoUnexpectedFailures = captureUnexpectedFailures(page);
  await login(page);

  const apiState = await page.evaluate(async () => {
    const [agentsResponse, costResponse] = await Promise.all([
      fetch("/api/config/agents", { credentials: "include" }),
      fetch("/api/cost", { credentials: "include" }),
    ]);
    const agents = await agentsResponse.json();
    const cost = await costResponse.json();
    return {
      agentStatus: agentsResponse.status,
      agentCount: agents.length,
      costStatus: costResponse.status,
      hierarchyRows: cost.by_hierarchy_tier?.length ?? 0,
      attributedCalls: cost.hierarchy_coverage?.attributed_calls ?? 0,
    };
  });
  expect(apiState).toMatchObject({ agentStatus: 200, agentCount: 60, costStatus: 200, hierarchyRows: 3 });
  expect(apiState.attributedCalls).toBeGreaterThan(0);

  const closeMetrics = page.getByTestId("close-metrics");
  if (await closeMetrics.isVisible()) await closeMetrics.click();
  const closeFloorplan = page.getByTestId("close-floorplan");
  if (await closeFloorplan.isVisible()) await closeFloorplan.click();
  await page.getByTestId("open-org-chart").click();
  await expect(page.getByRole("heading", { name: /Org Chart.*60 Agents/ })).toBeVisible();

  const agentButton = page.getByRole("button", { name: /Lisa.*hierarchy tier 2/ });
  await agentButton.focus();
  await expect(agentButton).toBeFocused();
  await agentButton.press("Enter");

  const editor = page.getByTestId("view-agent-editor");
  const tierSelect = editor.getByTestId("ae-hierarchy-tier");
  await expect(editor.getByTestId("ae-select")).toHaveValue("2");
  await expect(tierSelect).toHaveValue("2");

  try {
    await tierSelect.selectOption("3");
    await editor.getByTestId("ae-save").click();
    await expect.poll(() => liveAgentTier(page, 2), { timeout: 20_000 }).toBe(3);
    await page.reload();
    await login(page);
    await page.getByTestId("open-org-chart").click();
    const updatedButton = page.getByRole("button", { name: /Lisa.*hierarchy tier 3/ });
    await expect(updatedButton).toBeVisible();
    await updatedButton.click();
    await expect(page.getByTestId("view-agent-editor").getByTestId("ae-hierarchy-tier")).toHaveValue("3");
    await page.screenshot({
      path: "evidence/issue-395-live/live-desktop-org-chart-agent-editor.png",
      fullPage: true,
    });
  } finally {
    await updateLiveAgentTier(page, 2, 2);
  }

  await page.reload();
  await login(page);
  await page.getByTestId("open-org-chart").click();
  await expect(page.getByRole("button", { name: /Lisa.*hierarchy tier 2/ })).toBeVisible();
  expect(await liveAgentTier(page, 2)).toBe(2);
  expect(await page.evaluate((key) => document.body.innerText.includes(key), dashboardKey!)).toBe(false);
  expect(await page.evaluate((key) => Object.values(localStorage).some((value) => value === key), dashboardKey!)).toBe(false);
  assertNoUnexpectedFailures();
});

test("live narrow org chart has no document overflow", async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  const assertNoUnexpectedFailures = captureUnexpectedFailures(page);
  await login(page);
  await page.getByTestId("tab-org-chart").click();
  await expect(page.getByText(/Org Chart.*60 Agents/)).toBeVisible();
  await expect(page.getByTestId("org-agent-node").first()).toBeVisible();
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= document.documentElement.clientWidth)).toBe(true);
  await page.screenshot({ path: "evidence/issue-395-live/live-mobile-org-chart.png", fullPage: true });
  assertNoUnexpectedFailures();
});
