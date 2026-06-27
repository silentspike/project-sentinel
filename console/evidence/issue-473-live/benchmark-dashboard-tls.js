#!/usr/bin/env node
// VM-local benchmark helper for Issue #473.
// Measures TLS appconnect and WebTransport ready latency against the live service.

const { execFileSync } = require("child_process");
const fs = require("fs");
const path = require("path");

const playwrightModule = process.env.PLAYWRIGHT_MODULE || "/home/ubuntu/pw-473/node_modules/playwright";
const { chromium } = require(playwrightModule);

const mode = process.env.MODE || "production";
const baseUrl = process.env.BASE_URL || "https://localhost:8001";
const samples = Number(process.env.SAMPLES || "20");
const outDir = process.env.OUT_DIR || "/tmp/issue-473-live";
const dashboardEnv = process.env.DASHBOARD_ENV || "/opt/sentinel/config/dashboard-backend.env";
const profileHome = process.env.PROFILE_HOME || "";
const browserExecutable = process.env.BROWSER_EXECUTABLE || "";
const extraBrowserArgs = (process.env.EXTRA_BROWSER_ARGS || "").split(/\s+/).filter(Boolean);
const hostResolverRules = process.env.HOST_RESOLVER_RULES || "";
const curlCacert = process.env.CURL_CACERT || "";
const curlInsecure = process.env.CURL_INSECURE === "true";
const ignoreHttpsErrors = process.env.IGNORE_HTTPS_ERRORS === "true";

function readApiKey() {
  const program = '$1=="SENTINEL_DASHBOARD_API_KEY" {print substr($0,index($0,"=")+1)}';
  const value = execFileSync("sudo", ["awk", "-F=", program, dashboardEnv], {
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  }).trim();
  if (!value) throw new Error(`SENTINEL_DASHBOARD_API_KEY was not found in ${dashboardEnv}`);
  return value;
}

function percentile(values, p) {
  const sorted = [...values].sort((a, b) => a - b);
  const idx = Math.min(sorted.length - 1, Math.ceil((p / 100) * sorted.length) - 1);
  return sorted[idx];
}

function summarize(values) {
  return {
    samples: values.length,
    p50_ms: Number(percentile(values, 50).toFixed(2)),
    p95_ms: Number(percentile(values, 95).toFixed(2)),
    min_ms: Number(Math.min(...values).toFixed(2)),
    max_ms: Number(Math.max(...values).toFixed(2)),
  };
}

function runCurlHandshake() {
  const values = [];
  for (let i = 0; i < samples; i += 1) {
    const args = ["-sS", "-o", "/dev/null", "-w", "%{time_appconnect}", "--http1.1"];
    if (curlInsecure) args.push("-k");
    if (curlCacert) args.push("--cacert", curlCacert);
    args.push(`${baseUrl}/api/health`);
    const out = execFileSync("curl", args, { encoding: "utf8" }).trim();
    values.push(Number(out) * 1000);
  }
  return values;
}

function browserEnv() {
  if (!profileHome) return process.env;
  return { ...process.env, HOME: profileHome };
}

async function runWebTransportBenchmark(apiKey) {
  const launchOptions = {
    headless: true,
    args: ["--no-sandbox", ...extraBrowserArgs],
    env: browserEnv(),
  };
  if (hostResolverRules) launchOptions.args.push(`--host-resolver-rules=${hostResolverRules}`);
  if (browserExecutable) launchOptions.executablePath = browserExecutable;
  const forbiddenArgs = launchOptions.args.filter((arg) =>
    /ignore-certificate|allow-insecure|certificate-errors/i.test(arg)
  );
  if (mode === "production" && forbiddenArgs.length > 0) {
    throw new Error(`Production browser requested certificate bypass flags: ${forbiddenArgs.join(",")}`);
  }

  const browser = await chromium.launch(launchOptions);
  const context = await browser.newContext({
    ignoreHTTPSErrors: ignoreHttpsErrors,
    viewport: { width: 1280, height: 900 },
  });
  const page = await context.newPage();
  await page.goto(baseUrl, { waitUntil: "domcontentloaded", timeout: 20000 });
  if (await page.getByTestId("login-key").count()) {
    await page.getByTestId("login-key").fill(apiKey);
    await page.getByTestId("login-submit").click();
    await page.getByTestId("shell").waitFor({ timeout: 20000 });
  }
  const result = await page.evaluate(async (n) => {
    function base64ToArrayBuffer(base64) {
      const binary = atob(base64);
      const bytes = new Uint8Array(binary.length);
      for (let i = 0; i < binary.length; i += 1) bytes[i] = binary.charCodeAt(i);
      return bytes.buffer;
    }
    const certHashResponse = await fetch("/api/cert-hash");
    const certHashJson = await certHashResponse.json();
    const certHash = certHashJson && certHashJson.hash ? certHashJson.hash : null;
    const options = {};
    if (certHash) {
      options.serverCertificateHashes = [{
        algorithm: "sha-256",
        value: base64ToArrayBuffer(certHash),
      }];
    }
    const values = [];
    for (let i = 0; i < n; i += 1) {
      const ticketResponse = await fetch("/api/wt-ticket", { credentials: "same-origin" });
      if (!ticketResponse.ok) throw new Error(`wt-ticket status ${ticketResponse.status}`);
      const ticketJson = await ticketResponse.json();
      const t0 = performance.now();
      const transport = new WebTransport(`${window.location.origin}/?t=${encodeURIComponent(ticketJson.ticket)}`, options);
      await transport.ready;
      values.push(performance.now() - t0);
      transport.close();
      await transport.closed.catch(() => {});
    }
    return { cert_hash_present: Boolean(certHash), values };
  }, samples);
  await context.close();
  await browser.close();
  return result;
}

async function main() {
  fs.mkdirSync(outDir, { recursive: true });
  const apiKey = readApiKey();
  console.log(`mode=${mode}`);
  console.log(`base_url=${baseUrl}`);
  console.log(`samples=${samples}`);
  console.log(`ignore_https_errors=${ignoreHttpsErrors}`);
  console.log(`curl_insecure=${curlInsecure}`);
  console.log(`curl_cacert=${curlCacert || "<none>"}`);
  console.log(`requested_browser_args=${JSON.stringify(["--no-sandbox", ...extraBrowserArgs, ...(hostResolverRules ? [`--host-resolver-rules=${hostResolverRules}`] : [])])}`);
  const tlsValues = runCurlHandshake();
  const wt = await runWebTransportBenchmark(apiKey);
  const output = {
    mode,
    base_url: baseUrl,
    samples,
    tls_handshake_ms: summarize(tlsValues),
    webtransport_connect_ms: summarize(wt.values),
    cert_hash_present: wt.cert_hash_present,
  };
  const outputPath = path.join(outDir, `${mode}-benchmark.json`);
  fs.writeFileSync(outputPath, `${JSON.stringify(output, null, 2)}\n`);
  console.log(JSON.stringify(output, null, 2));
}

main().catch((error) => {
  console.error(error && error.stack ? error.stack : String(error));
  process.exit(1);
});
