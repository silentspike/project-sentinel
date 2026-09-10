import { expect, test } from "@playwright/test";

const identity = { schema_version: 1, principal_id: "customer-one", tenant_id: "tenant-one", customer_id: "customer-one" };
const request = { request_id: "request-one", summary_ref: "Studio website", desired_outcome: "Three accessible pages", constraints: ["No tracking"], state: "submitted", version: 1, proposal_ids: [], clarifications: [], feedback: [] };

test("customer workspace sends only customer commands and preserves ambiguous operations across reload", async ({ page }, testInfo) => {
  await page.setViewportSize({ width: 1440, height: 900 });
  const paths: string[] = [];
  const commands: unknown[] = [];
  let fail = true;
  let submitted = false;
  await page.route("**/api/**", async route => {
    const path = new URL(route.request().url()).pathname;
    paths.push(path);
    if (path === "/api/customer/status") return route.fulfill({ json: { authenticated: true, identity } });
    if (path === "/api/customer/overview") return route.fulfill({ json: { requests: submitted ? [request] : [], proposals: [] } });
    if (path === "/api/customer/commands") {
      commands.push(route.request().postDataJSON());
      submitted = true;
      if (fail) { fail = false; return route.abort("failed"); }
      return route.fulfill({ json: { request } });
    }
    return route.fulfill({ status: 404, json: {} });
  });
  await page.goto("/?view=customer");
  await page.getByLabel("Projekttitel").fill("Studio website");
  await page.getByLabel("Gewuenschtes Ergebnis").fill("Three accessible pages");
  await page.getByLabel("Rahmenbedingungen").fill("No tracking");
  await page.getByRole("button", { name: "Anfrage senden", exact: true }).click();
  await expect(page.getByRole("alert")).toBeVisible();
  await page.reload();
  await expect(page.getByText("Bestaetigung ausstehend", { exact: true })).toBeVisible();
  await expect(page.getByRole("button", { name: "Anfrage senden", exact: true })).toBeDisabled();
  await page.getByRole("button", { name: "Gleiche Anfrage erneut pruefen" }).click();
  await expect(page.getByText("Bestaetigung ausstehend", { exact: true })).toHaveCount(0);
  expect(commands).toHaveLength(2);
  expect(commands[1]).toEqual(commands[0]);
  expect(paths.every(path => path.startsWith("/api/customer/"))).toBe(true);
  expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBe(1440);
  await page.screenshot({ path: testInfo.outputPath("customer-workspace.png"), fullPage: true });
  for (const width of [768, 1024]) {
    await page.setViewportSize({ width, height: 900 });
    expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBe(width);
    await expect(page.getByRole("button", { name: "Abmelden" })).toBeInViewport();
  }
});

test("customer login never persists the credential and remains separate from operator authentication", async ({ page }) => {
  const credential = "customer-credential-01234567890123456789";
  let signedIn = false;
  const paths: string[] = [];
  await page.route("**/api/**", async route => {
    const path = new URL(route.request().url()).pathname;
    paths.push(path);
    if (path.endsWith("/status")) return route.fulfill({ json: { authenticated: signedIn, ...(signedIn ? { identity } : {}) } });
    if (path.endsWith("/login")) {
      expect(route.request().postDataJSON()).toEqual({ key: credential });
      signedIn = true;
      return route.fulfill({ json: { authenticated: true, identity } });
    }
    if (path.endsWith("/logout")) {
      signedIn = false;
      return route.fulfill({ json: { authenticated: false } });
    }
    return route.fulfill({ json: { requests: [], proposals: [], projects: [] } });
  });
  await page.goto("/?view=customer");
  await page.getByLabel("Kundenschluessel").fill(credential);
  await page.getByRole("button", { name: "Anmelden", exact: true }).click();
  await expect(page.getByRole("button", { name: "Abmelden" })).toBeVisible();
  expect(await page.evaluate(() => JSON.stringify({ ...localStorage, ...sessionStorage }))).not.toContain(credential);
  await page.getByRole("button", { name: "Abmelden" }).click();
  await expect(page.getByLabel("Kundenschluessel")).toHaveValue("");
  expect(paths.every(path => path.startsWith("/api/customer/"))).toBe(true);
});

test("proposal acceptance carries exact version and digest only after confirmation", async ({ page }) => {
  let command: Record<string, unknown> | undefined;
  await page.route("**/api/customer/**", async route => {
    const path = new URL(route.request().url()).pathname;
    if (path.endsWith("status")) return route.fulfill({ json: { authenticated: true, identity } });
    if (path.endsWith("overview")) return route.fulfill({ json: {
      requests: [{ ...request, state: "proposed", version: 3, proposal_ids: ["proposal-one"] }],
      proposals: [{ proposal_id: "proposal-one", request_id: request.request_id, proposal_digest: "a".repeat(64), scope: "Three-page site", deliverables: ["Source and preview"], acceptance_criteria: ["Keyboard access"], exclusions: [], assumptions: [], cost_ceiling_micros: 1_000_000, expires_at_unix_ms: Date.now() + 60_000 }],
    } });
    command = route.request().postDataJSON();
    return route.fulfill({ json: { accepted: true } });
  });
  await page.goto("/?view=customer");
  page.once("dialog", dialog => dialog.dismiss());
  await page.getByRole("button", { name: "Angebot annehmen" }).click();
  expect(command).toBeUndefined();
  page.once("dialog", dialog => dialog.accept());
  await page.getByRole("button", { name: "Angebot annehmen" }).click();
  await expect.poll(() => command).toBeDefined();
  expect(command?.command).toEqual({ command: "accept_proposal", request_id: "request-one", expected_version: 3, proposal_id: "proposal-one", proposal_digest: "a".repeat(64) });
});
