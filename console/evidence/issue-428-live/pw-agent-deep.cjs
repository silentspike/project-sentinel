// #428 AC-2: drive the deployed AgentDeepView like a real user over an SSH tunnel to the
// loopback-only console. ignoreHTTPSErrors handles the self-signed cert. The console pushes the live
// agent list over WebTransport (QUIC/UDP), which does NOT survive a TCP SSH tunnel — so this headless
// run reaches one agent's deep view via the `#deep=<id>` deep-link fallback (the AgentsView-card click
// entry is covered by the vitest test). The FS browser + activity charts read REST (/api/control/...,
// /api/events), which tunnels fine. No lifecycle clicks here (Start/Stop/Remove verified via curl).
const { chromium } = require("playwright");

const path = require("node:path");
const { runPrivateSmoke } = require("../private-browser-smoke.cjs");

runPrivateSmoke(chromium, async ({ browser, key, output, url, agentId }) => {
  url.hash = "deep=" + encodeURIComponent(agentId);
  const ctx = await browser.newContext({ ignoreHTTPSErrors: true, viewport: { width: 1600, height: 1000 } });
  const page = await ctx.newPage();
  await page.goto(url.href, { waitUntil: "domcontentloaded", timeout: 30000 });

  await page.getByTestId("login-key").fill(key);
  await page.getByTestId("login-submit").click();
  await page.waitForTimeout(2500);

  // Open the Agent Deep View panel via the toolbar; it reads the `#deep=<id>` hash on mount.
  const t0 = Date.now();
  await page.getByTestId("open-agent-deep").click({ timeout: 15000 });
  await page.waitForSelector('[data-testid="view-agent-deep"]', { timeout: 15000 });
  await page.waitForSelector('[data-testid="deep-agent-name"]', { timeout: 15000 });
  const openMs = Date.now() - t0;


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

  await page.screenshot({ path: path.join(output, "pw-agent-deep.png"), fullPage: true });

  console.log("OPEN_MS=" + openMs);
  console.log("AGENT_VIEW_LOADED=true");
  console.log("SPARK_POINTS_LEN=" + sparkPoints);
  console.log("DONUT_LEGEND=" + donutLegend + " DONUT_CIRCLES=" + donutSegments);
  console.log("FS_ENTRIES=" + fsEntries);
  console.log("FS_DEDUP_PRESENT=" + Boolean(dedupText.trim()));
}, { tunnelUrl: "https://localhost:8001/", agent: true });
