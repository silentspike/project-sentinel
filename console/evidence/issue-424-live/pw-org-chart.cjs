// #424 AC-1/AC-2/AC-3: drive the deployed OrgChartView like a real user over an SSH tunnel to the
// loopback-only console. ignoreHTTPSErrors handles the self-signed cert. The view uses REST
// (apiJson GET /api/config/agents), so it works without WebTransport.
const { chromium } = require("playwright");

const OUT = "/work/company/ps-424-org-chart/console/evidence/issue-424-live";

(async () => {
  const browser = await chromium.launch({ headless: true });
  const ctx = await browser.newContext({ ignoreHTTPSErrors: true, viewport: { width: 1600, height: 1000 } });
  const page = await ctx.newPage();
  await page.goto("https://localhost:8001/", { waitUntil: "domcontentloaded", timeout: 30000 });

  await page.getByTestId("login-key").fill(process.env.DKEY || "");
  await page.getByTestId("login-submit").click();
  await page.waitForTimeout(2500);

  // Open the Org Chart panel; measure render time (open -> first agent node present).
  const t0 = Date.now();
  await page.getByTestId("open-org-chart").click({ timeout: 15000 });
  await page.waitForSelector('[data-testid="view-org-chart"]', { timeout: 15000 });
  await page.waitForFunction(
    () => document.querySelectorAll('[data-testid="org-agent-node"]').length > 0,
    { timeout: 15000 },
  );
  const renderMs = Date.now() - t0;

  const agentNodes = await page.locator('[data-testid="org-agent-node"]').count();
  const deptCount = await page.locator('[data-testid="org-dept"]').count();
  const roleCount = await page.locator('[data-testid="org-role"]').count();
  const tiers = (await page.locator('[data-testid="org-tier"]').allTextContents()).map((s) => s.trim());
  const dashCount = tiers.filter((t) => t.includes("—")).length;

  await page.screenshot({ path: OUT + "/pw-org-chart.png", fullPage: true });

  // AC-3: click the first agent node -> Agent Editor opens pre-selected. Measure click->editor latency.
  const firstNode = page.locator('[data-testid="org-agent-node"]').first();
  const c0 = Date.now();
  await firstNode.click();
  await page.waitForSelector('[data-testid="view-agent-editor"]', { timeout: 15000 });
  await page.waitForFunction(
    () => {
      const sel = document.querySelector('[data-testid="ae-select"]');
      return sel && sel.value && sel.value !== "";
    },
    { timeout: 15000 },
  );
  const clickMs = Date.now() - c0;
  const preselected = await page.locator('[data-testid="ae-select"]').inputValue();

  await page.screenshot({ path: OUT + "/pw-org-chart-editor.png", fullPage: true });

  console.log("AGENT_NODES=" + agentNodes);
  console.log("DEPT_COUNT=" + deptCount + " ROLE_COUNT=" + roleCount);
  console.log("TIER_DASH_COUNT=" + dashCount + " of " + tiers.length);
  console.log("RENDER_MS=" + renderMs);
  console.log("CLICK_TO_EDITOR_MS=" + clickMs);
  console.log("EDITOR_PRESELECTED_ID=" + preselected);

  await browser.close();
})().catch((e) => {
  console.error("ERR " + e.message);
  process.exit(1);
});
