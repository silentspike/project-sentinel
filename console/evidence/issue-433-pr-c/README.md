# Issue #433 PR-C Evidence: Event Views

Date: 2026-06-02
Branch: feat/issue-433-pr-c-event-views
VM: ubuntu@10.0.0.240

## Scope

- Migrated Activity, Chaos, and Chat from the Bun dashboard to SolidJS console panels.
- Added one backend `event_log` topic using the existing topic+msgpack+zstd codec path.
- Added WebTransport connect backfill plus append-only deltas from `events.db`.
- Kept #464 CAS block-delta out of this PR. CAS remains a separate PR after #433.
- Removed the old Bun Activity, Chaos, and Chat view paths after screenshot parity verification.

## Live Smoke

Source: `smoke-summary.json`

- Old Bun baseline:
  - Activity: 200 Events
  - Chaos: 100 Events
  - Chat messages rendered: 100
- New SolidJS console:
  - Transport status: connected
  - Activity before append wait: 9946 Events
  - Activity after append wait: 10000 Events
  - Chaos: 446 Events
  - Chat: 500 Nachrichten
  - Chat messages rendered in viewport: 22
- Append evidence:
  - Before: 9946
  - After: 10000
  - Increased: true

## Screenshots

- `old-activity.png`
- `new-activity.png`
- `old-chaos.png`
- `new-chaos.png`
- `old-chat.png`
- `new-chat.png`

## Checks

- `cargo remote --no-copy-lock -- clippy --workspace --all-targets -- -D warnings`
- `cargo remote --no-copy-lock -- test --workspace`
- `cd console && bunx vitest run`
- `cd console && bunx tsc --noEmit`
- `cd console && bunx vite build`
- `cargo remote --no-copy-lock -- test -p sentinel-dashboard-backend`
- `cd dashboard && bun run typecheck`
- `cd dashboard && bun test`
- `git diff --check`

## Bun Removal Smoke

After screenshot parity verification, the old Bun paths for Activity, Chaos, and Chat were removed and deployed to the VM Bun service.

- `/public/js/activity.js`: 404
- `/public/js/chaos.js`: 404
- `/public/js/chat.js`: 404
- `/api/activity`: 404
- `/api/chaos`: 404
- `/api/chat`: 404
- VM service state after smoke:
  - `sentinel-dashboard`: active
  - `sentinel-dashboard-backend`: active
  - `sentinel-gateway`: inactive
  - `sentinel-judge`: inactive
  - `sentinel-health-monitor.timer`: inactive
  - `sentinel-health-monitor.service`: inactive
