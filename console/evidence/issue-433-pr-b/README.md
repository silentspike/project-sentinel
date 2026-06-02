# Issue #433 PR-B Push Views Evidence

Date: 2026-06-02
VM: `ubuntu@10.0.0.240`
Branch: `feat/issue-433-pr-b-push-views`

## Scope

SolidJS console migration for:

- `agents`
- `floorplan`
- `metrics`
- `cockpit`

The old Bun view paths for these views were removed after live screenshot verification:

- removed nav/sections from `dashboard/public/index.html`
- removed imports/render/update paths from `dashboard/public/js/app.js`
- deleted `dashboard/public/js/{agents,floorplan,metrics,cockpit}.js`
- deleted the direct old `floorplan.js` DOM test

The old Bun API routes remain for now because non-migrated Bun views still use shared endpoints.

## Frontend Gates

```text
cd console && bunx tsc --noEmit
  PASS

cd console && bunx vitest run
  PASS: 14/14

cd console && bunx vite build
  PASS
```

Dashboard fallback for remaining, not-yet-migrated views:

```text
cd dashboard && bun run typecheck
  PASS

cd dashboard && bun test
  PASS: 83/83
```

No backend files were changed in PR-B, so no remote Rust rebuild was required for this PR.

## Live VM Smoke

After deploying `console/dist` to `/opt/sentinel/console-dist` and restarting `sentinel-dashboard-backend`:

```text
sentinel-dashboard-backend: active
sentinel-dashboard: active
sentinel-gateway: inactive
sentinel-judge: inactive
sentinel-health-monitor.timer: inactive
sentinel-health-monitor.service: inactive
```

Playwright Chromium ran directly on the VM against loopback:

- old Bun: `http://127.0.0.1:8000`
- new console: `https://127.0.0.1:8001` with `ignoreHTTPSErrors`

New console live counts from the screenshot run:

```json
{
  "agents": 60,
  "rooms": 26,
  "hasKpi": true,
  "incidents": 200
}
```

Gateway-dependent metrics degraded visibly as expected (`Gateway offline`) with Gateway inactive.

## Screenshots

Before/after pairs:

- `old-agents.png` / `new-agents.png`
- `old-floorplan.png` / `new-floorplan.png`
- `old-metrics.png` / `new-metrics.png`
- `old-cockpit.png` / `new-cockpit.png`

The first new screenshot pass found and fixed three UI issues before this evidence was finalized:

- Agents progress showed `231%` when `agent_live` contained 60 rows.
- Metrics showed raw epoch bucket text.
- Cockpit SLO/incident layout clipped in narrow tiles.
