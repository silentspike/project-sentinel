// #427 AC-4: drive the deployed CostView like a real user over an SSH tunnel to the
// loopback-only console. ignoreHTTPSErrors handles the self-signed cert (SAN lists
// localhost). CostView uses REST (apiJson GET /api/cost), so it works without WebTransport.
const { chromium } = require("playwright");

const OUT = "/work/company/ps-427-cost/console/evidence/issue-427-live";

(async () => {
  const browser = await chromium.launch({ headless: true });
  const ctx = await browser.newContext({ ignoreHTTPSErrors: true, viewport: { width: 2400, height: 1000 } });
  const page = await ctx.newPage();
  await page.goto("https://localhost:8001/", { waitUntil: "domcontentloaded", timeout: 30000 });

  await page.getByTestId("login-key").fill(process.env.DKEY || "");
  await page.getByTestId("login-submit").click();
  await page.waitForTimeout(2500);

  // Open the Cost panel from the tiling toolbar, then wait through one 10s poll.
  await page.getByTestId("open-cost").click({ timeout: 15000 });
  await page.waitForSelector('[data-testid="view-cost"]', { timeout: 15000 });
  await page.waitForTimeout(11000);

  const agentRows = await page.locator('[data-testid="cost-agent-row"]').count();
  const tierRows = await page.locator('[data-testid="cost-tier-row"]').count();
  const cacheReads = (await page.locator('[data-testid="cost-cache-read"]').allTextContents())
    .map((s) => s.trim()).filter((s) => s && s !== "0");
  const cacheCreations = (await page.locator('[data-testid="cost-cache-creation"]').allTextContents())
    .map((s) => s.trim()).filter((s) => s && s !== "0");
  const sparkPoints = await page.locator('[data-testid="cost-sparkline"] polyline').getAttribute("points");

  await page.screenshot({ path: OUT + "/pw-cost-view.png", fullPage: true });

  console.log("AGENT_ROWS=" + agentRows);
  console.log("TIER_ROWS=" + tierRows);
  console.log("CACHE_READ_NONZERO=" + cacheReads.length + " sample=" + JSON.stringify(cacheReads.slice(0, 4)));
  console.log("CACHE_CREATION_NONZERO=" + cacheCreations.length + " sample=" + JSON.stringify(cacheCreations.slice(0, 4)));
  console.log("SPARKLINE_POINTS_LEN=" + ((sparkPoints || "").length));

  await browser.close();
})().catch((e) => {
  console.error("ERR " + e.message);
  process.exit(1);
});
