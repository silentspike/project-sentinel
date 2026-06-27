// #428 AC-2: drive the deployed AgentDeepView like a real user over an SSH tunnel to the
// loopback-only console. ignoreHTTPSErrors handles the self-signed cert. The console pushes the live
// agent list over WebTransport (QUIC/UDP), which does NOT survive a TCP SSH tunnel — so this headless
// run reaches one agent's deep view via the `#deep=<id>` deep-link fallback (the AgentsView-card click
// entry is covered by the vitest test). The FS browser + activity charts read REST (/api/control/...,
// /api/events), which tunnels fine. No lifecycle clicks here (Start/Stop/Remove verified via curl).
const { chromium } = require("playwright");

const OUT = "/work/company/ps-428-agent-deep-view/console/evidence/issue-428-live";
const AGENT_ID = process.env.DEEP_AGENT_ID || "8";

(async () => {
  const browser = await chromium.launch({ headless: true });
  const ctx = await browser.newContext({ ignoreHTTPSErrors: true, viewport: { width: 1600, height: 1000 } });
  const page = await ctx.newPage();
  await page.goto("https://localhost:8001/#deep=" + AGENT_ID, { waitUntil: "domcontentloaded", timeout: 30000 });

  await page.getByTestId("login-key").fill(process.env.DKEY || "");
  await page.getByTestId("login-submit").click();
  await page.waitForTimeout(2500);

  // Open the Agent Deep View panel via the toolbar; it reads the `#deep=<id>` hash on mount.
  const t0 = Date.now();
  await page.getByTestId("open-agent-deep").click({ timeout: 15000 });
  await page.waitForSelector('[data-testid="view-agent-deep"]', { timeout: 15000 });
  await page.waitForSelector('[data-testid="deep-agent-name"]', { timeout: 15000 });
  const openMs = Date.now() - t0;

  const agentName = (await page.locator('[data-testid="deep-agent-name"]').textContent())?.trim();
  const statusText = (await page.locator('[data-testid="deep-status"]').textContent())?.trim();

  // Activity: sparkline polyline points + tool-donut legend entries from real event data.
  await page.waitForFunction(
    () => {
      const p = document.querySelector('[data-testid="deep-sparkline"] polyline');
      return p && (p.getAttribute("points") || "").length > 0;
    },
    { timeout: 15000 },
  ).catch(() => {});
  const sparkPoints = await page.evaluate(() => {
    const p = document.querySelector('[data-testid="deep-sparkline"] polyline');
    return p ? (p.getAttribute("points") || "").length : 0;
  });
  const donutLegend = await page.locator('[data-testid="deep-donut-legend"]').count();
  const donutSegments = await page.locator('[data-testid="deep-donut"] circle').count();
  const fsEntries = await page.locator('[data-testid="fs-entry"]').count();
  const dedupText = (await page.locator('[data-testid="fs-dedup"]').textContent().catch(() => "")) || "";

  await page.screenshot({ path: OUT + "/pw-agent-deep.png", fullPage: true });

  console.log("OPEN_MS=" + openMs);
  console.log("AGENT_NAME=" + agentName);
  console.log("STATUS_TEXT=" + statusText);
  console.log("SPARK_POINTS_LEN=" + sparkPoints);
  console.log("DONUT_LEGEND=" + donutLegend + " DONUT_CIRCLES=" + donutSegments);
  console.log("FS_ENTRIES=" + fsEntries);
  console.log("FS_DEDUP=" + dedupText.trim());

  await browser.close();
})().catch((e) => {
  console.error("ERR " + e.message);
  process.exit(1);
});
