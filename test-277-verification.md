# Issue #277 Verification

Date: 2026-05-28
Repo: `/work/company/project-sentinel`
Branch: `feat/issues-276-277-combined`
Deploy target: `ubuntu@10.0.0.240`

## Hardware Context

- Host: `sentinel-ubuntu-2404`
- CPU: Intel Core i7-3930K @ 3.20 GHz, Sandy Bridge-E, 2011, KVM, 8 vCPU
- Benchmark policy: Deploy-VM only, same-machine before/after. No local benchmark and no cargo-remote benchmark.
- Gateway policy: `cortex-gateway` stayed `inactive` for token protection. Expected 502s from `/api/control/traffic-stats` are not #277 failures.
- PR hygiene: final combined PR excludes unrelated commits `8e8ecf1` (Wasmtime audit), `bfac43c` (Go toolchain), and `d44f89e` (sandbox snapshot).

## Implementation Verified

- Dashboard poll path now calls one change-detection query: `SELECT last_event_id FROM projection_watermarks WHERE projection_name = ?`.
- Query plan on VM:

```text
QUERY PLAN
`--SEARCH projection_watermarks USING INDEX sqlite_autoindex_projection_watermarks_1 (projection_name=?)
```

- `sentinel-projection` owns `projection_watermarks`, updates it once per committed projection batch, and bootstraps the row from existing read-model data on DB open.
- `getGlobalMaxEventId()` falls back to `agent_live_view` + `room_live_view` + `kpi_1m` only for pre-watermark databases.
- Dashboard skips WebSocket change detection and health DB reads while `clients.size === 0`.

## Optimization Loop

| Variant | Result | Decision |
|---|---:|---|
| EventStore `projection_offsets` lookup | >100k `pread64` / 10s under tab load due mixed EventStore/health traffic | rejected |
| `agent_live_view` + `room_live_view` + `kpi_1m` MAX union, no KPI index | 615 `pread64` / 10s, `kpi_1m` had 36k+ buckets | rejected |
| Indexed MAX union | 18 `pread64` / 10s | rejected in favor of one-row watermark |
| `projection_watermarks` + idle skip | 1 SQL lookup per active poll, 0 Projection-DB reads when no clients | selected |

## Benchmark Evidence

Measurement command shape:

```text
vmstat 1 12
mpstat 1 12
iostat -x 1 12
/opt/sentinel/dashboard/scripts/measure-ws-polling.sh 10
```

| Scenario | Reference | `pread64` / 10s | mpstat average | Logs |
|---|---|---:|---|---|
| Before, idle dashboard | pre-#277 dashboard baseline from previous stacked branch head `d44f89e` | 13 | user 0.15%, system 0.19%, iowait 0.01%, idle 99.61% | `/tmp/issue277-before-ws-only` |
| After, one-row watermark before idle skip | branch intermediate | 12 | user 0.15%, system 0.21%, iowait 0.01%, idle 99.49% | `/tmp/issue277-after-watermark-ws-only` |
| After, final idle skip | final branch | 11 | user 0.16%, system 0.18%, iowait 0.00%, idle 99.64% | `/tmp/issue277-after-idle-skip-ws-only` |
| After, 3 Playwright tabs active | final branch | 18 | user 0.17%, system 0.21%, iowait 0.01%, idle 99.57% | `/tmp/issue277-after-3tabs` |

Interpretation: `pread64` is a syscall proxy, not a SQL query counter. `strace -yy` in the final idle state showed remaining calls on `/proc/<pid>/statm`, not `projection.db`. With an active WebSocket client, `strace -yy` showed EventStore reads from the designed 5s health-lag update and no Projection-DB scan.

## AC Evidence

| AC | Result | Evidence |
|---|---|---|
| AC-1 | PASS | One Projection watermark SQL lookup per active poll; old per-view max exports removed; VM query plan is a primary-key lookup. |
| AC-2 | PASS | `ws-polling.test.ts` proves agent, room, cockpit, chaos, and activity broadcasts on watermark change; browser showed Bio bars and floorplan. |
| AC-3 | PASS | `ws-polling.test.ts` asserts the five message types and agent/room payload arrays. |
| AC-4 | PASS | `resetWatermarks()` test forces another full update; restore path still calls it from control routes. |
| AC-5 | PASS | `cd dashboard && bun test` -> 74 pass, 652 assertions; `cd dashboard && bun run typecheck` -> PASS. |
| AC-6 | PASS | 3 Playwright tabs for 30s, no `ERR_INSUFFICIENT_RESOURCES`; only expected gateway-off `502 /api/control/traffic-stats` console entries. |

## Live Deploy Evidence

```text
systemctl is-active sentinel-projection -> active
systemctl is-active sentinel-dashboard -> active
systemctl is-active cortex-gateway -> inactive
curl -fsS http://127.0.0.1:8000/api/health -> {"status":"ok","projection_lag":0}
curl -fsS http://127.0.0.1:8000/api/agents | jq length -> 38
sqlite projection_watermarks -> sentinel-projection|10662949
```

Playwright artifacts:

```text
/tmp/277-tab-a.png
/tmp/277-tab-a-floorplan.png
.playwright-cli/console-2026-05-28T19-16-14-627Z.log
.playwright-cli/console-2026-05-28T19-16-14-630Z.log
.playwright-cli/console-2026-05-28T19-16-14-673Z.log
```
