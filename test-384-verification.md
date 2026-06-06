# Verification Report - Issue #384: Time-Travel Debugging UI

**Issue:** #384 - Time-Travel Debugging UI: snapshot navigation + restore flow in the Dashboard (TOGAF Cluster 11)
**Branch:** `feat/issue-384-timetravel-ui`
**Date:** 2026-05-29
**Scope:** frontend/dashboard work only (`dashboard/`). Backend (WorldSnapshot,
Hot-Swap Restore, Tiered Retention) already exists and was not touched.

## Summary of Changes

| File | Change |
|-------|-----------|
| `dashboard/src/types.ts` | Types `SnapshotWorldState`, `SnapshotRoomOccupancy` |
| `dashboard/src/db.ts` | `getSnapshotWorldState()` - world state by event replay up to `last_event_id` |
| `dashboard/src/routes/control.ts` | `GET /api/control/snapshot-state` |
| `dashboard/public/js/timetravel.js` | **NEW** - Time Travel view: timeline, world state, restore |
| `dashboard/public/index.html` | Nav button + `<section id="view-timetravel">` |
| `dashboard/public/js/app.js` | Wiring + fix for the broken `snapshot_restored` handler (AC-4) |
| `dashboard/public/css/style.css` | Timeline/detail panel styles |
| `dashboard/src/__tests__/control.test.ts` | 3 tests for `snapshot-state` |

## Static Verification

```
$ bun run typecheck     → tsc --noEmit, EXIT 0 (keine Fehler)
$ bun test              → 78 pass, 0 fail, 673 expect() calls (11 Dateien)
```
On the deploy VM after deploy, also `bun run typecheck` -> EXIT 0.

## Architecture Decision AC-2 (World State Derivation)

`SnapshotMeta` (the snapshot listing) carries **no** agent/room counts, and the
snapshot payload is a bincode-2 BLOB (not decodable in Bun). Therefore the
world state at the snapshot time is **deterministically derived from the EventStore**
(which the Dashboard reads read-only anyway) - no new Rust endpoint, no gateway,
no LLM call (token-safe):

- **`active_agent_count`** = `agent_count` from the nearest `tick_snapshot` event
  at/before `tick`. Authoritative and identical to the live agents view.
- **`present_agent_count`** + **room occupancy** = replay of the `agent_spawned` /
  `transit_completed` / `agent_despawned` events up to `last_event_id` (agents in the
  building including off-shift, with their last known room).

---

## Acceptance Criteria - Evidence

### AC-1: UI lists snapshots with tier badge, timestamp, tick, and size along a timeline

**Status: PASS**

API evidence (real VM data):
```
$ curl -s http://10.0.0.240:8000/api/control/snapshots
[{"id":"019e7409-...","tier":"hourly","tick":1476529,"sim_hour":21.745188,
  "payload_size_bytes":566324,"created_at_ms":1780063232607}, ... 59 Snapshots]
```
Playwright screenshot `issue384-ac1-timeline.png`: visual timeline with
axis labels (17.3.2026 -> 29.5.2026), tier-coded markers (hourly green,
monthly orange), legend, and snapshot list (59) with columns for tier badge,
timestamp, tick, sim hour, and size.

### AC-2: Selecting a snapshot shows the world state (agent/room counts) for that point in time

**Status: PASS**

API evidence (snapshot tick 1476529):
```
$ curl -s "http://10.0.0.240:8000/api/control/snapshot-state?snapshot_id=019e7409-625d-7703-95eb-2bee0815a7c3"
{"snapshot_id":"019e7409-...","tier":"hourly","tick":1476529,"last_event_id":10735662,
 "active_agent_count":26,"present_agent_count":43,"room_count":12,
 "rooms":[{"room_id":"buero-dev-1","name":"Entwicklungsbüro 1","occupant_count":9}, ...]}
```
Consistency check: `active_agent_count` (26) == live `/api/agents` active agents (26).
Sum of room occupancy (9+6+6+4+3+3+3+2+2+2+2+1) == `present_agent_count` (43).

Unit test `control.test.ts` "derives agent and room counts at the snapshot boundary":
verifies replay boundary (`last_event_id`), despawn handling, events after the
boundary are ignored, room name resolution. PASS.

Playwright screenshots `issue384-ac2-worldstate.png` (26 active / 43 in building /
12 occupied rooms + meta) and `issue384-ac2-rooms.png` (per-room occupancy with
umlaut-correct room names + restore button).

### AC-3: Restore button triggers Hot-Swap Restore via existing API (with confirm dialog)

**Status: PASS**

Confirm dialog verified on click (Playwright: click -> dialog appears ->
`dialog-dismiss` cancels, no restore). Restore then triggered;
Daemon journal (10.0.0.240) confirms the full Hot-Swap path through the
existing Operator API:
```
INFO sentinel_daemon::operator_api: Restore via Operator-API angefordert snapshot_id=019e7409-...
INFO sentinel_daemon::orchestrator: Hot-Swap Restore gestartet snapshot_id=019e7409-...
INFO sentinel_daemon::orchestrator: Pre-Restore Snapshot erstellt (Rollback-Punkt) snapshot_id=019e7442-...
INFO sentinel_daemon::orchestrator: Agent-Prozesse fuer Restore terminiert terminated=26
INFO sentinel_daemon::orchestrator: Projection Snapshot-Seeding abgeschlossen agents_seeded=26 rooms_seeded=11
INFO sentinel_daemon::orchestrator: Hot-Swap Restore abgeschlossen snapshot_id=019e7409-... tick=1476529 agents=26
```
Restore is reversible - the daemon automatically creates a pre-restore rollback snapshot.

### AC-4: After restore, the Dashboard updates live (WebSocket Event)

**Status: PASS**

Latent bug fixed: the previous `snapshot_restored` handler in `app.js` called
undefined functions `loadAgents()`/`loadRooms()` (ReferenceError) - replaced
by `reloadAfterRestore()` (fetch `/api/agents` + `/api/rooms`, re-render).

Runtime evidence via spy WebSocket on `/ws` (Playwright `eval`): frame sequence
received after restore:
```
["health_update","snapshot_restored","agent_update","room_update",
 "cockpit_update","chaos_update","activity_update"]
```
The server broadcasts `snapshot_restored`; `resetWatermarks()` forces the
following full `agent_update`/`room_update` broadcast. Playwright screenshot
`issue384-ac4-live-agents.png`: Agents view shows the restored state without
manual reload (`autonomy:bio_emergency T1476609`, directly after
restore tick 1476529).

### AC-5: Playwright verification: select snapshot, trigger restore, screenshot

**Status: PASS**

Complete E2E run against `http://10.0.0.240:8000` with playwright-cli:
navigate to Time Travel tab -> select snapshot marker -> world state -> confirm dialog ->
restore -> live update. Screenshots: `issue384-ac1-timeline.png`,
`issue384-ac2-worldstate.png`, `issue384-ac2-rooms.png`,
`issue384-ac3-restore-feedback.png`, `issue384-ac4-live-agents.png`
(in `/home/jan/Pictures/Screenshots/`).

---

## Deploy + Health (Deploy VM 10.0.0.240)

```
$ grep ExecStart /etc/systemd/system/sentinel-dashboard.service
  ExecStart=/usr/local/bin/bun run start   (WorkingDirectory=/opt/sentinel/dashboard, PORT=8000)
$ sudo systemctl restart sentinel-dashboard → active, "Dashboard running on http://localhost:8000"
$ systemctl is-active sentinel-dashboard sentinel-daemon sentinel-projection → active/active/active
$ journalctl -u sentinel-daemon (3 min) → 0 panics / FATAL
```
`cortex-gateway` remained INACTIVE as required (token protection); #384 needs no
gateway and no LLM calls.

## Not Tested / Notes

- Tier filter/search UI: not in scope (#384 only requires navigation + restore).
- `buero-design` without a ROOM_METADATA entry gracefully falls back to the room_id
  (existing config drift, not part of this issue).

## Confidence

**95%** - all 5 ACs are evidenced with command+output or screenshot in the running system
(backend API against real data, Hot-Swap Restore in the daemon journal, live update via
WS frame). Remaining doubt only around long-term UI robustness when the
snapshot count grows significantly (timeline marker density).
