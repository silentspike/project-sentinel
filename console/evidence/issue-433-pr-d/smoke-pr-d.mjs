import { chromium } from "playwright";
import fs from "node:fs/promises";
import http from "node:http";
import https from "node:https";
import path from "node:path";

const oldKey = process.env.OLD_DASHBOARD_KEY ?? process.env.DASHBOARD_KEY;
const newKey = process.env.NEW_DASHBOARD_KEY ?? process.env.DASHBOARD_KEY;
if (!oldKey || !newKey) {
  throw new Error("OLD_DASHBOARD_KEY and NEW_DASHBOARD_KEY are required");
}

const outDir = process.env.OUT_DIR ?? "/tmp/issue-433-pr-d-screens";
const chromePath = process.env.CHROME_PATH
  ?? "/home/ubuntu/.cache/ms-playwright/chromium-1223/chrome-linux64/chrome";

await fs.mkdir(outDir, { recursive: true });

const browser = await chromium.launch({
  executablePath: chromePath,
  headless: true,
  args: ["--no-sandbox", "--disable-dev-shm-usage"],
});

async function screenshot(locator, fileName) {
  await locator.screenshot({ path: path.join(outDir, fileName) });
}

async function postLogin(baseUrl, key) {
  const url = new URL("/api/auth/login", baseUrl);
  const body = JSON.stringify({ key });
  const client = url.protocol === "https:" ? https : http;
  return new Promise((resolve, reject) => {
    const req = client.request({
      hostname: url.hostname,
      port: url.port,
      path: url.pathname,
      method: "POST",
      rejectUnauthorized: false,
      headers: {
        "Content-Type": "application/json",
        "Content-Length": Buffer.byteLength(body),
      },
    }, (res) => {
      res.resume();
      res.on("end", () => resolve({
        status: res.statusCode ?? 0,
        setCookie: res.headers["set-cookie"] ?? [],
      }));
    });
    req.on("error", reject);
    req.write(body);
    req.end();
  });
}

async function loginContext(context, baseUrl, key) {
  const result = await postLogin(baseUrl, key);
  if (result.status < 200 || result.status >= 300) {
    throw new Error(`login failed for ${baseUrl}: ${result.status}`);
  }
  const cookieLine = result.setCookie.find((entry) => entry.startsWith("sentinel_session="));
  if (!cookieLine) throw new Error(`login for ${baseUrl} did not return sentinel_session`);
  const token = cookieLine.split(";")[0].slice("sentinel_session=".length);
  await context.addCookies([{
    name: "sentinel_session",
    value: token,
    domain: "127.0.0.1",
    path: "/",
    httpOnly: true,
    secure: baseUrl.startsWith("https:"),
    sameSite: "Strict",
  }]);
}

async function activateOldView(page, viewName) {
  await page.evaluate((name) => {
    document.querySelectorAll(".nav-btn").forEach((button) => {
      button.classList.toggle("active", button.getAttribute("data-view") === name);
    });
    document.querySelectorAll(".view").forEach((view) => {
      view.classList.toggle("active", view.id === `view-${name}`);
    });
  }, viewName);
}

const evidence = {
  old: {},
  new: {},
  tokenGate: {},
};

try {
  const oldContext = await browser.newContext({
    viewport: { width: 1440, height: 1000 },
  });
  await loginContext(oldContext, "http://127.0.0.1:8000", oldKey);
  const oldPage = await oldContext.newPage();
  await oldPage.goto("http://127.0.0.1:8000", { waitUntil: "domcontentloaded" });

  await oldPage.waitForSelector("#view-timetravel", { timeout: 20000 });
  await activateOldView(oldPage, "timetravel");
  await oldPage.waitForSelector("#view-timetravel.active", { timeout: 15000 });
  await oldPage.waitForTimeout(1000);
  await screenshot(oldPage.locator("#view-timetravel"), "old-timetravel.png");
  evidence.old.snapshots = await oldPage.locator("#view-timetravel").innerText().catch(() => "");

  await activateOldView(oldPage, "control");
  await oldPage.waitForSelector("#view-control.active", { timeout: 15000 });
  await oldPage.waitForTimeout(1000);
  await screenshot(oldPage.locator("#view-control"), "old-control.png");
  evidence.old.control = await oldPage.locator("#view-control").innerText().catch(() => "");
  await oldContext.close();

  const newContext = await browser.newContext({
    ignoreHTTPSErrors: true,
    viewport: { width: 760, height: 1040 },
  });
  await loginContext(newContext, "https://127.0.0.1:8001", newKey);
  const page = await newContext.newPage();
  await page.goto("https://127.0.0.1:8001", { waitUntil: "domcontentloaded" });
  await page.waitForSelector('[data-testid="shell"]', { timeout: 20000 });
  await page.waitForSelector('[data-testid="bottom-tabbar"]', { timeout: 20000 });
  await page.waitForFunction(() => {
    const text = document.querySelector('[data-testid="ctl-liveindicator"]')?.textContent ?? "";
    return text.includes("connected");
  }, { timeout: 20000 }).catch(() => undefined);
  evidence.new.transport = await page.locator('[data-testid="ctl-liveindicator"]').first().innerText().catch(() => "");

  await page.locator('[data-testid="tab-control"]').click();
  const control = page.locator('[data-testid="view-control"]').last();
  await control.waitFor({ state: "visible", timeout: 15000 });
  await page.waitForSelector('[data-testid="control-gateway"]', { timeout: 15000 });
  await screenshot(control, "new-control.png");
  evidence.new.gateway = await page.locator('[data-testid="control-gateway"]').last().innerText();
  evidence.new.controlText = await control.innerText();

  await page.locator('[data-testid="tab-timetravel"]').click();
  const timetravel = page.locator('[data-testid="view-timetravel"]').last();
  await timetravel.waitFor({ state: "visible", timeout: 15000 });
  await page.waitForSelector('[data-testid="snapshot-count"]', { timeout: 15000 });
  await page.waitForTimeout(1000);
  await screenshot(timetravel, "new-timetravel.png");
  evidence.new.snapshotCount = await page.locator('[data-testid="snapshot-count"]').last().innerText();
  evidence.new.hasSnapshotDetail = await page.locator('[data-testid="snapshot-detail"]').count();
  evidence.new.timetravelText = await timetravel.innerText();

  await newContext.close();
} finally {
  await browser.close();
}

await fs.writeFile(
  path.join(outDir, "smoke-summary.json"),
  JSON.stringify(evidence, null, 2),
);

console.log(JSON.stringify(evidence, null, 2));
