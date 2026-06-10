// Issue #381 live smoke: Profiling-Panel auf der Deploy-VM (https://10.0.0.240:8001).
// Ausfuehrung AUF der VM (playwright + chromium dort installiert), Muster issue-433-pr-d.
//
//   DASHBOARD_KEY=<operator-key> node smoke-381.mjs
//
// Schritte: Login -> Profiling-Panel oeffnen -> auf phase-row-persist warten
// -> Screenshots (Panel + Fullpage). Screenshots werden von der Hauptsession
// visuell gesichtet + funktional bedient (Maintainer-Regel) und hier committed.

import { chromium } from "playwright";
import fs from "node:fs/promises";
import path from "node:path";

const key = process.env.DASHBOARD_KEY;
if (!key) throw new Error("DASHBOARD_KEY is required");

const baseUrl = process.env.BASE_URL ?? "https://10.0.0.240:8001";
const outDir = process.env.OUT_DIR ?? "/tmp/issue-381-screens";
const chromePath = process.env.CHROME_PATH
  ?? "/home/ubuntu/.cache/ms-playwright/chromium-1223/chrome-linux64/chrome";

await fs.mkdir(outDir, { recursive: true });

const browser = await chromium.launch({
  executablePath: chromePath,
  headless: true,
  args: ["--no-sandbox", "--disable-dev-shm-usage"],
});

try {
  const context = await browser.newContext({ ignoreHTTPSErrors: true, viewport: { width: 1440, height: 900 } });
  const page = await context.newPage();

  await page.goto(baseUrl, { waitUntil: "domcontentloaded" });

  // Operator-Login ueber die echte Login-Maske (user-like behavior).
  await page.getByTestId("login-key").fill(key);
  await page.getByTestId("login-submit").click();
  await page.waitForSelector("[data-testid=open-profiling], [data-testid=view-profiling]", { timeout: 15_000 });

  // Profiling-Panel oeffnen (Desktop-Tiling: openPanel-Button).
  const opener = page.locator("[data-testid=open-profiling]");
  if (await opener.count()) await opener.first().click();
  await page.waitForSelector("[data-testid=view-profiling]", { timeout: 10_000 });

  // Kernzustand: echte Phase-Zeilen aus dem laufenden Daemon (Warmup beachten).
  await page.waitForSelector("[data-testid=phase-row-persist]", { timeout: 60_000 });

  const panel = page.locator("[data-testid=view-profiling]");
  await panel.screenshot({ path: path.join(outDir, "profiling-panel.png") });
  await page.screenshot({ path: path.join(outDir, "profiling-fullpage.png"), fullPage: true });

  const rows = await page.locator("[data-testid^=phase-row-]").count();
  console.log(JSON.stringify({ ok: true, phase_rows: rows, out: outDir }));
  if (rows < 10) throw new Error(`expected 10 phase rows, got ${rows}`);
} finally {
  await browser.close();
}
