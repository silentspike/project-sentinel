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

const outDir = process.env.OUT_DIR ?? "/tmp/issue-433-pr-c-screens";
const chromePath = process.env.CHROME_PATH
  ?? "/home/ubuntu/.cache/ms-playwright/chromium-1223/chrome-linux64/chrome";

await fs.mkdir(outDir, { recursive: true });

const browser = await chromium.launch({
  executablePath: chromePath,
  headless: true,
  args: ["--no-sandbox", "--disable-dev-shm-usage"],
});

function countFromText(text) {
  const match = String(text ?? "").match(/\d+/);
  return match ? Number(match[0]) : 0;
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
  if (!cookieLine) {
    throw new Error(`login for ${baseUrl} did not return sentinel_session`);
  }
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

async function screenshot(locator, fileName) {
  await locator.screenshot({ path: path.join(outDir, fileName) });
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
  eventLogAppend: {},
};

try {
  const oldContext = await browser.newContext({
    viewport: { width: 1440, height: 1000 },
  });
  await loginContext(oldContext, "http://127.0.0.1:8000", oldKey);
  const oldPage = await oldContext.newPage();
  await oldPage.goto("http://127.0.0.1:8000", { waitUntil: "domcontentloaded" });

  await oldPage.waitForSelector("#view-activity #activity-list", { state: "attached", timeout: 20000 });
  await activateOldView(oldPage, "activity");
  await oldPage.waitForSelector("#view-activity.active #activity-list", { timeout: 15000 });
  await screenshot(oldPage.locator("#view-activity"), "old-activity.png");
  evidence.old.activity = await oldPage.locator("#activity-count").innerText().catch(() => "");

  await oldPage.waitForSelector("#view-chaos #chaos-list", { state: "attached", timeout: 20000 });
  await activateOldView(oldPage, "chaos");
  await oldPage.waitForSelector("#view-chaos.active #chaos-list", { timeout: 15000 });
  await screenshot(oldPage.locator("#view-chaos"), "old-chaos.png");
  evidence.old.chaos = await oldPage.locator(".chaos-count").innerText().catch(() => "");

  await oldPage.waitForSelector("#view-chat #chat-list", { state: "attached", timeout: 20000 });
  await activateOldView(oldPage, "chat");
  await oldPage.waitForSelector("#view-chat.active #chat-list", { timeout: 15000 });
  await screenshot(oldPage.locator("#view-chat"), "old-chat.png");
  evidence.old.chatMessages = await oldPage.locator("#chat-list .chat-message").count();
  await oldContext.close();

  const newContext = await browser.newContext({
    ignoreHTTPSErrors: true,
    viewport: { width: 720, height: 1000 },
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
  evidence.new.status = await page.locator('[data-testid="ctl-liveindicator"]').first().innerText().catch(() => "");

  await page.locator('[data-testid="tab-activity"]').click();
  const activity = page.locator('[data-testid="view-activity"]').last();
  await activity.waitFor({ state: "visible", timeout: 15000 });
  await page.waitForFunction(() => {
    const text = document.querySelector('[data-testid="activity-count"]')?.textContent ?? "";
    return /\d+/.test(text);
  }, { timeout: 20000 });
  const activityBeforeText = await page.locator('[data-testid="activity-count"]').last().innerText();
  await screenshot(activity, "new-activity.png");

  await page.locator('[data-testid="tab-chaos"]').click();
  const chaos = page.locator('[data-testid="view-chaos"]').last();
  await chaos.waitFor({ state: "visible", timeout: 15000 });
  await page.waitForSelector('[data-testid="chaos-count"]', { timeout: 15000 });
  await screenshot(chaos, "new-chaos.png");
  evidence.new.chaos = await page.locator('[data-testid="chaos-count"]').last().innerText();

  await page.locator('[data-testid="tab-chat"]').click();
  const chat = page.locator('[data-testid="view-chat"]').last();
  await chat.waitFor({ state: "visible", timeout: 15000 });
  await page.waitForSelector('[data-testid="chat-count"]', { timeout: 15000 });
  await screenshot(chat, "new-chat.png");
  evidence.new.chat = await page.locator('[data-testid="chat-count"]').last().innerText();
  evidence.new.chatMessages = await chat.locator('[data-testid="chat-message"]').count();

  await page.locator('[data-testid="tab-activity"]').click();
  await page.waitForSelector('[data-testid="activity-count"]', { timeout: 15000 });
  const before = countFromText(activityBeforeText);
  let afterText = activityBeforeText;
  for (let i = 0; i < 60; i += 1) {
    await page.waitForTimeout(1000);
    afterText = await page.locator('[data-testid="activity-count"]').last().innerText();
    if (countFromText(afterText) > before) break;
  }
  const after = countFromText(afterText);
  evidence.new.activityBefore = activityBeforeText;
  evidence.new.activityAfter = afterText;
  evidence.eventLogAppend = {
    before,
    after,
    increased: after > before,
  };

  await newContext.close();
} finally {
  await browser.close();
}

await fs.writeFile(
  path.join(outDir, "smoke-summary.json"),
  JSON.stringify(evidence, null, 2),
);

console.log(JSON.stringify(evidence, null, 2));
