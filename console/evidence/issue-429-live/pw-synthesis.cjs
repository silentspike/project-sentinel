// #429 AC-5: drive the deployed SynthesisView like a real user over an SSH tunnel to the
// loopback-only console. ignoreHTTPSErrors handles the self-signed cert (SAN already lists
// localhost). The view uses REST (apiJson), so it works without WebTransport.
const { chromium } = require("playwright");

(async () => {
  const browser = await chromium.launch({ headless: true });
  const ctx = await browser.newContext({ ignoreHTTPSErrors: true });
  const page = await ctx.newPage();
  await page.goto("https://localhost:8001/", { waitUntil: "domcontentloaded", timeout: 30000 });

  // Login.
  await page.getByTestId("login-key").fill(process.env.DKEY || "");
  await page.getByTestId("login-submit").click();
  await page.waitForTimeout(2500);

  // Open the Synthesis panel from the tiling toolbar.
  await page.getByTestId("open-synthesis").click({ timeout: 15000 });
  await page.waitForSelector('[data-testid="view-synthesis"]', { timeout: 15000 });

  // Wait through one 10s poll so rules + traffic-responses + judge-alerts load.
  await page.waitForTimeout(11000);

  const rulesCount = await page.locator('[data-testid^="synthesis-rule-"]').count();
  const rowCount = await page.locator('[data-testid="inspector-row"]').count();
  const decisions = (await page.locator('[data-testid="inspector-decision"]').allTextContents())
    .map((s) => s.trim()).filter(Boolean);
  const judge = (await page.locator('[data-testid="inspector-judge"]').allTextContents())
    .map((s) => s.trim()).filter(Boolean);

  await page.screenshot({ path: "evidence/issue-429-live/pw-synthesis-view.png", fullPage: true });

  // Functional: toggle bio_bladder off and confirm the checkbox state flips, then restore.
  // Only when rules are present (gateway up); skip gracefully when gateway is down.
  let before = null, after = null;
  if (rulesCount > 0) {
    const bb = page.getByTestId("synthesis-rule-bio_bladder");
    before = await bb.isChecked();
    await bb.click();
    await page.waitForTimeout(1500);
    after = await bb.isChecked();
    await page.screenshot({ path: "evidence/issue-429-live/pw-synthesis-toggled.png", fullPage: true });
    await bb.click(); // restore
    await page.waitForTimeout(800);
  }

  console.log("RULES_RENDERED=" + rulesCount);
  console.log("INSPECTOR_ROWS=" + rowCount);
  console.log("DECISIONS=" + JSON.stringify(decisions.slice(0, 8)));
  console.log("JUDGE_CELLS=" + JSON.stringify(judge.slice(0, 8)));
  console.log("TOGGLE_before_after=" + before + "/" + after);

  await browser.close();
})().catch((e) => {
  console.error("ERR " + e.message);
  process.exit(1);
});
