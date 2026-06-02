// Test-Fixture (NICHT ausgeliefert): serviert dist/ + minimale /api-Mocks fuer die optische
// playwright-cli-Verifikation der Shell ohne erreichbares #431-Backend. Auth togglebar via PREVIEW_AUTHED.
// Echte Live-Transport-/Auth-Verifikation laeuft im gemeinsamen Phase-2-Smoke gegen das deployte Backend.
import { serve } from "bun";
import { join, normalize } from "node:path";
import { existsSync, readFileSync } from "node:fs";

const DIST = join(import.meta.dir, "..", "dist");
const AUTHED = (process.env.PREVIEW_AUTHED ?? "true") !== "false";
const PORT = Number(process.env.PREVIEW_PORT ?? 4173);

const TYPES: Record<string, string> = { ".html": "text/html", ".js": "text/javascript", ".css": "text/css", ".map": "application/json", ".svg": "image/svg+xml" };

serve({
  port: PORT,
  fetch(req) {
    const url = new URL(req.url);
    const p = url.pathname;
    if (p === "/api/auth/status") return Response.json({ authenticated: AUTHED });
    if (p === "/api/auth/login") return Response.json({ authenticated: true }, { headers: { "set-cookie": "sentinel_session=mock; HttpOnly; Path=/" } });
    if (p === "/api/cert-hash") return Response.json({ algorithm: "sha-256", hash: null });
    if (p === "/api/agents") return Response.json({ agents: [] });
    // Static aus dist/ (SPA-Fallback auf index.html)
    let rel = normalize(p === "/" ? "/index.html" : p).replace(/^(\.\.[/\\])+/, "");
    let file = join(DIST, rel);
    if (!existsSync(file)) file = join(DIST, "index.html");
    const ext = file.slice(file.lastIndexOf("."));
    return new Response(readFileSync(file), { headers: { "content-type": TYPES[ext] ?? "application/octet-stream" } });
  },
});
console.log(`preview-mock on http://127.0.0.1:${PORT} (authed=${AUTHED})`);
