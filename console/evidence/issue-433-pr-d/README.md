# Issue #433 PR-D Evidence: Proxy Views + Bun Removal

Date: 2026-06-02
Branch: feat/issue-433-pr-d-proxy-views
VM: ubuntu@10.0.0.240

## Scope

- Migrated Control and Timetravel from the Bun dashboard to SolidJS console panels.
- Added the missing `POST /api/control/platform-analyze` proxy in `sentinel-dashboard-backend`.
- Added public `GET /api/health` on the Rust console backend for deploy smoke and health monitoring.
- Removed the old Bun/Hono dashboard tree and `sentinel-dashboard.service` after screenshot parity verification.
- Updated CI, CodeQL, dependency/security workflows, Docker demo, deploy smoke, and health-monitor to target `console/` plus `sentinel-dashboard-backend` on `:8001`.

## Live Smoke

Source: `smoke-summary.json`

- Old Bun baseline:
  - Control rendered gateway-offline degraded state.
  - Timetravel rendered `Snapshots (166)`.
- New SolidJS console:
  - Transport status: `connected`
  - Control status: `Gateway offline`
  - Timetravel status: `166 Snapshots`
  - Snapshot detail panel rendered: yes
- 0-token gate during smoke:
  - `sentinel-gateway`: inactive
  - `sentinel-judge`: inactive
  - `sentinel-health-monitor.timer`: inactive
  - `sentinel-health-monitor.service`: inactive

## Screenshots

- `old-control.png`
- `new-control.png`
- `old-timetravel.png`
- `new-timetravel.png`
- `cutover-control.png`
- `cutover-timetravel.png`

## Checks

- `cargo remote --no-copy-lock -- clippy --workspace --all-targets -- -D warnings`
- `cargo remote --no-copy-lock -- test --workspace`
- `cargo remote --no-copy-lock -c release/sentinel-dashboard-backend -- build -p sentinel-dashboard-backend --release`
- `cd console && bunx tsc --noEmit`
- `cd console && bunx vitest run`
- `cd console && bunx vite build`
- `git diff --check`

## Bun Removal Smoke

After parity screenshots, the old Bun fallback was removed from source and then removed from the VM runtime path:

- `dashboard/`: deleted from repo
- `deploy/systemd/sentinel-dashboard.service`: deleted from repo
- VM `sentinel-dashboard.service`: stopped and disabled
- VM port `:8000`: no longer serving the old dashboard
- VM `sentinel-dashboard-backend.service`: active on `:8001`
- Cutover smoke (`smoke-pr-d-cutover.mjs`) after removal:
  - `GET /api/health`: 200 `{"service":"sentinel-dashboard-backend","status":"ok"}`
  - WebTransport status: `connected`
  - Control status: `Gateway offline`
  - Timetravel status: `166 Snapshots`
  - Snapshot detail panel rendered: yes
