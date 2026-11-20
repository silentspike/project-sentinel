// Issue #381 live smoke: Profiling-Panel auf der Deploy-VM.
// Ausfuehrung AUF der VM (playwright + chromium dort installiert), Muster issue-433-pr-d.
//
// Load BASE_URL and DASHBOARD_KEY from the private configuration before running.
//
// Schritte: Login -> Profiling-Panel oeffnen -> auf phase-row-persist warten
// -> Screenshots (Panel + Fullpage). Screenshots werden von der Hauptsession
// visuell gesichtet und funktional bedient. Aufnahmen echter Deployments bleiben privat.

import { chromium } from "playwright";
import fs from "node:fs/promises";
import path from "node:path";

const key = process.env.DASHBOARD_KEY;
if (!key?.trim()) throw new Error("DASHBOARD_KEY is required");

const baseUrl = process.env.BASE_URL;
if (!baseUrl?.trim()) throw new Error("BASE_URL is required");
let endpoint;
try {
  endpoint = new URL(baseUrl);
} catch {
  throw new Error("BASE_URL must be an HTTP or HTTPS URL");
}
if (!["http:", "https:"].includes(endpoint.protocol) || endpoint.username || endpoint.password) {
  throw new Error("BASE_URL must be an HTTP or HTTPS URL");
}
const outDir = process.env.OUT_DIR ?? "/tmp/issue-381-screens";
const chromePath = process.env.CHROME_PATH || chromium.executablePath();

try {
  await fs.mkdir(outDir, { recursive: true, mode: 0o700 });
  const output = await fs.lstat(outDir);
  if (!output.isDirectory() || output.uid !== process.getuid() || (output.mode & 0o077)) {
    throw new Error();
  }
} catch {
  throw new Error("OUT_DIR must be a private directory owned by the current user");
}

let browser;
try {
  browser = await chromium.launch({
    executablePath: chromePath,
    headless: true,
    args: ["--no-sandbox", "--disable-dev-shm-usage"],
  });
} catch {
  throw new Error("Chromium could not start. Check the Playwright installation or CHROME_PATH.");
}

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
  if (rows < 10) throw new Error("Profiling panel has fewer than 10 phase rows");
  console.log(JSON.stringify({ ok: true, phase_rows: rows }));
} catch {
  throw new Error("Profiling smoke failed. Verify the endpoint, operator access, and that the panel has at least 10 phase rows.");
} finally {
  await browser.close();
}
