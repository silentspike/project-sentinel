import { expect, test } from "@playwright/test";

const identity = { schema_version: 1, principal_id: "customer-one", tenant_id: "tenant-one", customer_id: "customer-one" };
const request = { request_id: "request-one", summary_ref: "Studio website", desired_outcome: "Three accessible pages", constraints: ["No tracking"], state: "submitted", version: 1, proposal_ids: [], clarifications: [], feedback: [] };

test("customer replies to the exact Sales question with durable retry", async ({ page }) => {
  const consultation = [{ message_id: "question-one", in_reply_to: null as string | null, content: "Which audience should the website address?", role: "sales" }];
  const writes: { operation_id: string; command: Record<string, unknown> }[] = [];
  await page.route("**/api/customer/**", async route => {
    const path = new URL(route.request().url()).pathname;
    if (path.endsWith("status")) return route.fulfill({ json: { authenticated: true, identity } });
    if (path.endsWith("overview")) return route.fulfill({ json: { requests: [{ ...request, state: "clarifying", version: 2, consultation }], proposals: [] } });
    expect(path).toBe("/api/customer/commands");
    writes.push(route.request().postDataJSON());
    if (writes.length === 1) return route.abort("failed");
    consultation.push({ message_id: "answer-one", in_reply_to: "question-one", content: "Local businesses", role: "customer" });
    return route.fulfill({ json: {} });
  });
  await page.goto("/?view=customer");
  await expect(page.getByText(consultation[0].content)).toBeVisible();
  await expect(page.getByRole("button", { name: "Antwort senden", exact: true })).toBeDisabled();
  await page.getByLabel("Ihre Antwort", { exact: true }).fill("Local businesses");
  await page.getByRole("button", { name: "Antwort senden", exact: true }).click();
  await expect(page.getByRole("button", { name: "Gleiche Anfrage erneut pruefen" })).toBeVisible();
  await page.reload();
  await page.getByRole("button", { name: "Gleiche Anfrage erneut pruefen" }).click();
  await expect(page.getByRole("button", { name: "Antwort senden", exact: true })).toHaveCount(0);
  expect(writes).toHaveLength(2);
  expect(writes[1]).toEqual(writes[0]);
  expect(writes[0].command).toEqual({ command: "send_customer_request_message", request_id: request.request_id, expected_version: 2, in_reply_to: "question-one", content: "Local businesses" });
  await expect(page.getByText("Local businesses", { exact: true })).toBeVisible();
});

test("delivery acceptance requires consent and replays the displayed version after reload", async ({ page }) => {
  const delivery = {
    delivery: { id: "delivery-one", generation: 1, digest: "a".repeat(64) },
    release: { id: "release-one", generation: 2, digest: "b".repeat(64) },
    state: "delivered", release_state: "active", issued_at_ms: Date.now(), expires_at_ms: Date.now() + 60000,
    preview_digest: "c".repeat(64),
  };
  const expected = { action: "confirm_delivery", project_id: "project-one", delivery: { ...delivery.delivery }, release: { ...delivery.release } };
  const writes: { operation_id: string; intent: unknown }[] = [];
  await page.route("**/api/customer/**", async route => {
    const path = new URL(route.request().url()).pathname;
    if (path.endsWith("status")) return route.fulfill({ json: { authenticated: true, identity } });
    if (path.endsWith("overview")) return route.fulfill({ json: { requests: [request], proposals: [], projects: [{ project_id: "project-one", request_id: request.request_id, state: "delivery_candidate", version: 2, work_items: [], deliveries: [delivery] }] } });
    expect(path).toBe("/api/customer/delivery");
    writes.push(route.request().postDataJSON());
    if (writes.length === 1) return route.abort("failed");
    delivery.state = "accepted";
    return route.fulfill({ json: {} });
  });
  await page.goto("/?view=customer");
  page.once("dialog", dialog => dialog.dismiss());
  await page.getByRole("button", { name: "Lieferung abnehmen" }).click();
  expect(writes).toEqual([]);
  page.once("dialog", async dialog => {
    expect(dialog.message()).toContain("delivery-one, Version 1");
    await dialog.accept();
  });
  await page.getByRole("button", { name: "Lieferung abnehmen" }).click();
  await expect(page.getByRole("button", { name: "Gleiche Anfrage erneut pruefen" })).toBeVisible();
  delivery.delivery.generation = 2;
  delivery.delivery.digest = "d".repeat(64);
  await page.reload();
  await expect(page.getByRole("button", { name: "Lieferung abnehmen" })).toBeDisabled();
  await page.getByRole("button", { name: "Gleiche Anfrage erneut pruefen" }).click();
  await expect(page.getByRole("button", { name: "Gleiche Anfrage erneut pruefen" })).toHaveCount(0);
  expect(writes).toHaveLength(2);
  expect(writes[0].intent).toEqual(expected);
  expect(writes[1]).toEqual(writes[0]);
  await expect(page.getByRole("button", { name: "Lieferung abnehmen" })).toBeDisabled();
});

test("delivery preview is isolated, inert, network-blocked and stable across overview refresh", async ({ page }, testInfo) => {
  await page.setViewportSize({ width: 1440, height: 1000 });
  const delivery = {
    delivery: { id: "delivery-one", generation: 1, digest: "a".repeat(64) },
    release: { id: "release-one", generation: 2, digest: "b".repeat(64) },
    state: "delivered", release_state: "active", issued_at_ms: Date.now(), expires_at_ms: Date.now() + 60000,
    preview_digest: "c".repeat(64),
  };
  const binding = { project_id: "project-one", delivery: delivery.delivery, release: delivery.release };
  const artifact = { artifact_id: "website-source_tree-0", digest: "d".repeat(64), media_type: "application/json" };
  const html = `<!doctype html><html><head><style>body{font:16px sans-serif;color:#162a24;padding:32px;background:#f3f8f5}h1{font-size:28px}details{padding:16px;border:1px solid #417b63}a{display:block;margin:24px 0}</style></head><body>
    <h1>Studio Website</h1><p>Verifizierte Lieferung</p><details><summary>Projektumfang</summary><p>Drei barrierefreie Seiten</p></details>
    <script>document.body.dataset.executed="yes";top.localStorage.setItem("preview-escaped","yes");fetch("https://preview-denied.invalid/script")</script>
    <img src="https://preview-denied.invalid/image"><iframe src="https://preview-denied.invalid/frame"></iframe>
    <a href="https://preview-denied.invalid/navigation">Externe Navigation</a>
    <form action="https://preview-denied.invalid/form"><button>Formular senden</button></form>
    </body></html>`;
  const outgoing: string[] = [], writes: string[] = [];
  const htmlBytes = new TextEncoder().encode(html);
  const encodedHtml = btoa(Array.from(htmlBytes, byte => String.fromCharCode(byte)).join(""));
  let overviewReads = 0;
  await page.route("https://preview-denied.invalid/**", async route => { outgoing.push(route.request().url()); await route.abort(); });
  await page.route("**/api/customer/**", async route => {
    const path = new URL(route.request().url()).pathname;
    if (path.endsWith("status")) return route.fulfill({ json: { authenticated: true, identity } });
    if (path.endsWith("overview")) {
      overviewReads++;
      return route.fulfill({ json: { requests: [request], proposals: [], projects: [{ project_id: "project-one", request_id: request.request_id, state: "delivery_candidate", version: 2, work_items: [], deliveries: [delivery] }] } });
    }
    if (path.endsWith("preview")) {
      const body = route.request().postDataJSON();
      if (!body.file) { expect(body).toEqual(binding); return route.fulfill({ json: { ...binding, manifest_digest: "e".repeat(64), artifacts: [artifact] } }); }
      expect(body).toEqual({ ...binding, file: { artifact_id: artifact.artifact_id, path: "index.html" } });
      return route.fulfill({ json: { ...binding, manifest_digest: "e".repeat(64), artifact_id: artifact.artifact_id, path: "index.html", encoding: "base64", size_bytes: htmlBytes.length, content: encodedHtml } });
    }
    writes.push(path); return route.fulfill({ status: 403, json: {} });
  });
  await page.goto("/?view=customer");
  await page.getByRole("button", { name: "Vorschau oeffnen" }).click();
  await expect(page.getByLabel("Artefakt")).toHaveValue(artifact.artifact_id);
  await page.getByRole("button", { name: "Vorschau laden" }).click();
  const outer = page.frameLocator('iframe[title="Isolierte Lieferungsvorschau"]');
  const inner = outer.frameLocator('iframe[title="Gelieferte Webseite"]');
  await expect(inner.getByRole("heading", { name: "Studio Website" })).toBeVisible();
  await expect(inner.locator("body")).toHaveCSS("background-color", "rgb(243, 248, 245)");
  await expect(inner.locator("body")).not.toHaveAttribute("data-executed", "yes");
  await inner.getByText("Projektumfang", { exact: true }).click();
  await expect(inner.getByText("Drei barrierefreie Seiten")).toBeVisible();
  await expect.poll(() => overviewReads).toBeGreaterThan(1);
  await expect(inner.getByText("Drei barrierefreie Seiten")).toBeVisible();
  await inner.getByRole("button", { name: "Formular senden" }).click();
  expect(outgoing).toEqual([]); expect(writes).toEqual([]);
  expect(await page.evaluate(() => localStorage.getItem("preview-escaped"))).toBeNull();
  expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBe(1440);
  await page.screenshot({ path: testInfo.outputPath("isolated-customer-preview.png"), fullPage: true });
  await inner.getByRole("link", { name: "Externe Navigation" }).click();
  await page.waitForTimeout(200);
  expect(outgoing).toEqual([]);
  expect(page.url()).toContain("view=customer");
  delivery.state = "changes_requested";
  await expect(page.getByText("Diese Vorschau ist nicht mehr verfuegbar.")).toBeVisible();
  await expect(page.locator('iframe[title="Isolierte Lieferungsvorschau"]')).toHaveCount(0);
  await page.getByRole("button", { name: "Schliessen", exact: true }).click();
  await expect(page.locator('iframe[title="Isolierte Lieferungsvorschau"]')).toHaveCount(0);
});

test("delivery overview displays the stored version without issuing an acceptance", async ({ page }) => {
  const writes: string[] = [];
  await page.route("**/api/customer/**", async route => {
    if (route.request().method() !== "GET") writes.push(route.request().url());
    if (new URL(route.request().url()).pathname.endsWith("status")) {
      return route.fulfill({ json: { authenticated: true, identity } });
    }
    return route.fulfill({ json: { requests: [request], proposals: [], projects: [{
      project_id: "project-one", request_id: request.request_id, state: "active", version: 5,
      work_items: [{ work_item_id: "website", state: "done" }],
      deliveries: [{
        delivery: { id: "delivery-website-one", generation: 7, digest: "a".repeat(64) },
        release: { id: "release-one", generation: 3, digest: "b".repeat(64) },
        state: "delivered", release_state: "active", issued_at_ms: 100, expires_at_ms: 200,
        preview_digest: "c".repeat(64),
      }],
    }] } });
  });
  await page.goto("/?view=customer");
  await expect(page.getByRole("heading", { name: "Lieferungen" })).toBeVisible();
  const row = page.getByRole("row").filter({ hasText: "delivery-website-one" });
  await expect(row.getByRole("cell").nth(0)).toContainText("delivery-website-one");
  await expect(row.getByRole("cell").nth(1)).toHaveText("7");
  await expect(row.getByRole("cell").nth(2)).toHaveText("delivered");
  expect(writes).toEqual([]);
});

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
