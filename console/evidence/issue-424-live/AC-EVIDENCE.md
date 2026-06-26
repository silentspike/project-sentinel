# #424 Org Chart View — Live-Verifikation (Deploy-VM 10.0.0.240) — Evidence

Reines Console-Feature: nur das statische Bundle nach `/opt/sentinel/console-dist` deployt (ServeDir),
KEIN Daemon/Gateway-Touch, KEIN Token-Risiko. SSH-Tunnel `-L 8001` + lokales playwright
(`ignoreHTTPSErrors`, Console loopback-only). Reale 60-Agent-PixelPerfekt-Config via `/api/config/agents`.

## AC-1 — Org Chart rendert die Hierarchie (department → role → agent)
`pw-org-chart.cjs` (`pw-org-chart.png`, visuell gesichtet):
```
AGENT_NODES=60  DEPT_COUNT=11  ROLE_COUNT=41
RENDER_MS=98
```
Visuell: "Org Chart — 60 Agents" + verschachtelter CSS-Tree (Abteilung fett → Rolle → Agent eingerueckt,
Reporting-Metadaten am Node). PASS

## AC-2 — Model-Tier pro Node sichtbar (read-only, #395 offen)
```
TIER_DASH_COUNT=60 of 60
```
Alle 60 Config-Agents haben `runtime.nano_runtime = null` (#395-Tier-Schema noch nicht gemergt), daher
zeigt jeder Node korrekt `tier: —` (read-only raw, NICHT geraten). vitest deckt zusaetzlich den Misch-Fall
ab (opus/sonnet/haiku-Werte + "—"-Fallback). PASS

## AC-3 — Klick auf Agent → Agent-Editor vorselektiert (#422)
```
CLICK_TO_EDITOR_MS=117
EDITOR_PRESELECTED_ID=53
```
`pw-org-chart-editor.png` (visuell gesichtet): Klick auf einen Org-Chart-Node oeffnet das Agent-Editor-Panel
**mit "Identity (id 53 — read-only)" vorselektiert** + editierbarem Formular. Der Mechanismus: shared
`selectedAgentId`-Signal (`state/selection.ts`) → `openPanel("agent-editor")` → AgentEditorView-Effect liest
+ **consume-and-clears** das Signal (kein stale Re-Apply beim Re-Open). PASS

## AC-4 — vitest + typecheck + build gruen
`bunx vitest run`: org-chart.test.ts 4 Tests (buildOrgTree-Gruppierung, Render+Tier-"—", Klick→selectedAgentId+openPanel,
consume-and-clear) + 55/55 total; `tsc --noEmit` exit 0; `vite build` exit 0. PASS

## Benchmark (Details: /work/company/BENCHMARK-REGISTER.md)
Live-Render 60 Agents = 98 ms, Klick→Editor = 117 ms (kein Jank). buildOrgTree-Compute 60=23µs / 250=101µs /
1000=353µs (linear, microsekunden — Daten-Transformation bottleneckt nie; statischer CSS-Tree-DOM dominiert).

## Deploy-Zustand
Nur Console-Bundle ersetzt (`/opt/sentinel/console-dist`, Backup `.bak-424`); daemon/projection/dashboard-backend
unangetastet, gateway/judge weiterhin inactive (kein LLM/Token). VM 1069 Sim ungestoert.
