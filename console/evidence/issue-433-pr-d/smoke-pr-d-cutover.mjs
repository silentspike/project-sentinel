import { chromium } from "playwright";
import fs from "node:fs/promises";
import https from "node:https";
import path from "node:path";

const key = process.env.DASHBOARD_KEY;
if (!key) {
  throw new Error("DASHBOARD_KEY is required");
}

const baseUrl = process.env.BASE_URL ?? "https://127.0.0.1:8001";
const outDir = process.env.OUT_DIR ?? "/tmp/issue-433-pr-d-cutover-screens";
const chromePath = process.env.CHROME_PATH
  ?? "/home/ubuntu/.cache/ms-playwright/chromium-1223/chrome-linux64/chrome";

await fs.mkdir(outDir, { recursive: true });

function request(method, pathname, body) {
  const url = new URL(pathname, baseUrl);
  const payload = body == null ? undefined : JSON.stringify(body);
  return new Promise((resolve, reject) => {
    const req = https.request({
      hostname: url.hostname,
      port: url.port,
      path: url.pathname,
      method,
      rejectUnauthorized: false,
      headers: payload
        ? {
            "Content-Type": "application/json",
            "Content-Length": Buffer.byteLength(payload),
          }
        : undefined,
    }, (res) => {
      const chunks = [];
      res.on("data", (chunk) => chunks.push(chunk));
      res.on("end", () => resolve({
        status: res.statusCode ?? 0,
        body: Buffer.concat(chunks).toString("utf8"),
        setCookie: res.headers["set-cookie"] ?? [],
      }));
    });
    req.on("error", reject);
    if (payload) req.write(payload);
    req.end();
  });
}

async function loginContext(context) {
  const result = await request("POST", "/api/auth/login", { key });
  if (result.status < 200 || result.status >= 300) {
    throw new Error(`login failed: ${result.status}`);
  }
  const cookieLine = result.setCookie.find((entry) => entry.startsWith("sentinel_session="));
  if (!cookieLine) throw new Error("login did not return sentinel_session");
  const token = cookieLine.split(";")[0].slice("sentinel_session=".length);
  await context.addCookies([{
    name: "sentinel_session",
    value: token,
    domain: "127.0.0.1",
    path: "/",
    httpOnly: true,
    secure: true,
    sameSite: "Strict",
  }]);
}

const browser = await chromium.launch({
  executablePath: chromePath,
  headless: true,
  args: ["--no-sandbox", "--disable-dev-shm-usage"],
});

const evidence = {
  health: null,
  transport: null,
  control: null,
  timetravel: null,
};

try {
  const health = await request("GET", "/api/health");
  evidence.health = { status: health.status, body: JSON.parse(health.body) };

  const context = await browser.newContext({
    ignoreHTTPSErrors: true,
    viewport: { width: 760, height: 1040 },
  });
  await loginContext(context);

  const page = await context.newPage();
  await page.goto(baseUrl, { waitUntil: "domcontentloaded" });
  await page.waitForSelector('[data-testid="shell"]', { timeout: 20000 });
  await page.waitForSelector('[data-testid="bottom-tabbar"]', { timeout: 20000 });
  await page.waitForFunction(() => {
    const text = document.querySelector('[data-testid="ctl-liveindicator"]')?.textContent ?? "";
    return text.includes("connected");
  }, { timeout: 20000 });
  evidence.transport = await page.locator('[data-testid="ctl-liveindicator"]').first().innerText();

  await page.locator('[data-testid="tab-control"]').click();
  const control = page.locator('[data-testid="view-control"]').last();
  await control.waitFor({ state: "visible", timeout: 15000 });
  await page.waitForSelector('[data-testid="control-gateway"]', { timeout: 15000 });
  await control.screenshot({ path: path.join(outDir, "cutover-control.png") });
  evidence.control = {
    gateway: await page.locator('[data-testid="control-gateway"]').last().innerText(),
    text: await control.innerText(),
  };

  await page.locator('[data-testid="tab-timetravel"]').click();
  const timetravel = page.locator('[data-testid="view-timetravel"]').last();
  await timetravel.waitFor({ state: "visible", timeout: 15000 });
  await page.waitForSelector('[data-testid="snapshot-count"]', { timeout: 15000 });
  await page.waitForTimeout(1000);
  await timetravel.screenshot({ path: path.join(outDir, "cutover-timetravel.png") });
  evidence.timetravel = {
    snapshotCount: await page.locator('[data-testid="snapshot-count"]').last().innerText(),
    hasSnapshotDetail: await page.locator('[data-testid="snapshot-detail"]').count(),
    text: await timetravel.innerText(),
  };

  await context.close();
} finally {
  await browser.close();
}

await fs.writeFile(
  path.join(outDir, "cutover-summary.json"),
  JSON.stringify(evidence, null, 2),
);

console.log(JSON.stringify(evidence, null, 2));
