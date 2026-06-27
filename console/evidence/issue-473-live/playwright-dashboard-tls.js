#!/usr/bin/env node
// Playwright evidence helper for Issue #473.
// Runs on the deployment VM. Production mode must not use certificate bypasses.

const fs = require("fs");
const path = require("path");
const { execFileSync } = require("child_process");

const playwrightModule = process.env.PLAYWRIGHT_MODULE || "/home/ubuntu/pw-381/node_modules/playwright";
const { chromium } = require(playwrightModule);

const mode = process.env.MODE || "zero-config";
const baseUrl = process.env.BASE_URL || "https://127.0.0.1:8001";
const outDir = process.env.OUT_DIR || "/tmp/issue-473-live";
const dashboardEnv = process.env.DASHBOARD_ENV || "/opt/sentinel/config/dashboard-backend.env";
const profileHome = process.env.PROFILE_HOME || "";
const headless = process.env.HEADLESS !== "false";
const browserExecutable = process.env.BROWSER_EXECUTABLE || "";
const extraBrowserArgs = (process.env.EXTRA_BROWSER_ARGS || "").split(/\s+/).filter(Boolean);
const hostResolverRules = process.env.HOST_RESOLVER_RULES || "";

function readApiKey() {
  const program = '$1=="SENTINEL_DASHBOARD_API_KEY" {print substr($0,index($0,"=")+1)}';
  const value = execFileSync("sudo", ["awk", "-F=", program, dashboardEnv], {
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  }).trim();
  if (!value) throw new Error(`SENTINEL_DASHBOARD_API_KEY was not found in ${dashboardEnv}`);
  return value;
}

function screenshotName(suffix) {
  return path.join(outDir, `${mode}-${suffix}.png`);
}

async function bodyText(page) {
  return page.locator("body").innerText({ timeout: 5000 }).catch(() => "");
}

function isCertWarning(text) {
  return /Your connection is not private|ERR_CERT_AUTHORITY_INVALID|NET::ERR_CERT_AUTHORITY_INVALID/i.test(text);
}

async function gotoAndCaptureWarning(page) {
  let gotoError = null;
  try {
    await page.goto(baseUrl, { waitUntil: "domcontentloaded", timeout: 20000 });
  } catch (error) {
    gotoError = String(error && error.message ? error.message : error);
  }
  const text = await bodyText(page);
  const warningDetected = isCertWarning(text) || (gotoError || "").includes("ERR_CERT_AUTHORITY_INVALID");
  console.log(`warning_detected=${warningDetected}`);
  if (gotoError) console.log(`warning_goto_error=${gotoError}`);
  await page.screenshot({ path: screenshotName("warning"), fullPage: true });
  if (!warningDetected) throw new Error("Expected Chromium certificate warning was not detected");
}

async function proceedPastChromiumWarning(page) {
  await page.keyboard.type("thisisunsafe", { delay: 10 });
  await page.waitForLoadState("domcontentloaded", { timeout: 20000 }).catch(() => {});
  try {
    await page.getByTestId("login-key").waitFor({ timeout: 8000 });
    return;
  } catch (_) {
    // Fall back to the visible interstitial controls when the keyboard shortcut is disabled.
  }
  try {
    if (await page.locator("#details-button").count()) {
      await page.locator("#details-button").click().catch(() => {});
    }
  } catch (_) {
  }
  try {
    if (await page.locator("#proceed-link").count()) {
      await page.locator("#proceed-link").click().catch(() => {});
    }
  } catch (_) {
  }
  await page.getByTestId("login-key").waitFor({ timeout: 20000 });
}

async function gotoProduction(page) {
  let gotoError = null;
  try {
    await page.goto(baseUrl, { waitUntil: "domcontentloaded", timeout: 20000 });
  } catch (error) {
    gotoError = String(error && error.message ? error.message : error);
  }
  const text = await bodyText(page);
  const warningDetected = isCertWarning(text) || (gotoError || "").includes("ERR_CERT_AUTHORITY_INVALID");
  console.log(`warning_detected=${warningDetected}`);
  if (gotoError) console.log(`production_goto_error=${gotoError}`);
  if (warningDetected || gotoError) {
    await page.screenshot({ path: screenshotName("certificate-failure"), fullPage: true }).catch(() => {});
    throw new Error("Production mode showed a certificate warning or navigation error");
  }
  await page.getByTestId("login-key").waitFor({ timeout: 20000 });
}

async function login(page, apiKey) {
  await page.getByTestId("login-key").fill(apiKey);
  await page.getByTestId("login-submit").click();
  await page.getByTestId("shell").waitFor({ timeout: 20000 });
}

async function waitForConnected(page) {
  await page.waitForFunction(() => {
    const el = document.querySelector('[data-testid="ctl-liveindicator"]');
    return el && el.textContent && el.textContent.trim() === "connected";
  }, null, { timeout: 45000 });
  const statusText = await page.getByTestId("ctl-liveindicator").first().innerText();
  console.log(`live_indicator=${JSON.stringify(statusText)}`);
}

async function readCertHash(page) {
  return page.evaluate(async () => {
    const response = await fetch("/api/cert-hash");
    return response.json();
  });
}

function browserEnv() {
  if (!profileHome) return process.env;
  return { ...process.env, HOME: profileHome };
}

async function main() {
  fs.mkdirSync(outDir, { recursive: true });
  const apiKey = readApiKey();
  const launchOptions = {
    headless,
    args: ["--no-sandbox", ...extraBrowserArgs],
    env: browserEnv(),
  };
  if (hostResolverRules) launchOptions.args.push(`--host-resolver-rules=${hostResolverRules}`);
  if (browserExecutable) launchOptions.executablePath = browserExecutable;
  console.log(`mode=${mode}`);
  console.log(`base_url=${baseUrl}`);
  console.log("ignore_https_errors=false");
  console.log(`headless=${headless}`);
  console.log(`profile_home=${profileHome || "<default>"}`);
  console.log(`browser_executable=${browserExecutable || "<playwright-default>"}`);
  console.log(`requested_browser_args=${JSON.stringify(launchOptions.args)}`);
  console.log("certificate_bypass_requested=false");
  const forbiddenArgs = launchOptions.args.filter((arg) =>
    /ignore-certificate|allow-insecure|certificate-errors/i.test(arg)
  );
  if (mode === "production" && forbiddenArgs.length > 0) {
    throw new Error(`Production browser requested certificate bypass flags: ${forbiddenArgs.join(",")}`);
  }
  const browser = await chromium.launch(launchOptions);
  const context = await browser.newContext({ viewport: { width: 1440, height: 1000 } });
  const page = await context.newPage();
  page.on("console", (msg) => console.log(`[browser:${msg.type()}] ${msg.text()}`));

  if (mode === "zero-config") {
    await gotoAndCaptureWarning(page);
    await proceedPastChromiumWarning(page);
  } else if (mode === "production") {
    await gotoProduction(page);
  } else {
    throw new Error(`Unsupported MODE: ${mode}`);
  }

  await login(page, apiKey);
  await waitForConnected(page);
  const certHash = await readCertHash(page);
  console.log(`cert_hash_json=${JSON.stringify(certHash)}`);
  console.log(`cert_hash_present=${Boolean(certHash && certHash.hash)}`);
  await page.screenshot({ path: screenshotName("connected"), fullPage: true });
  await context.close();
  await browser.close();
}

main().catch((error) => {
  console.error(error && error.stack ? error.stack : String(error));
  process.exit(1);
});
