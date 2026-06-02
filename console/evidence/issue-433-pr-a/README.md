# Issue #433 PR-A Backend Foundation Evidence

Date: 2026-06-02
VM: `ubuntu@10.0.0.240`
Branch: `feat/issue-433-pr-a-backend-foundation`

## Remote Build Gates

```text
cargo remote -c -- test -p sentinel-dashboard-backend
  PASS: unit 20/20
  PASS: tests/auth_routes.rs 5/5
  PASS: tests/resilience.rs 1/1
  PASS: tests/wt_roundtrip.rs 3/3

cargo remote -c -- clippy --workspace --all-targets -- -D warnings
  PASS

cargo remote -c -- build -p sentinel-dashboard-backend --release
  PASS: release profile finished
```

After correcting `/api/metrics/ebpf` degraded status from `gateway: offline` to `prometheus: offline`, both package tests and workspace clippy were rerun and passed.

## Deploy Smoke

```text
ExecStart=/opt/sentinel/bin/sentinel-dashboard-backend
sentinel-dashboard-backend: active
sentinel-gateway: inactive
sentinel-judge: inactive
sentinel-health-monitor.timer: inactive

Environment=SENTINEL_DASHBOARD_WT_BIND=0.0.0.0:8001
Environment=CORTEX_GATEWAY_PROXY_URL=http://127.0.0.1:8080
Environment=SENTINEL_DAEMON_PROMETHEUS_URL=http://127.0.0.1:9090
Environment=SENTINEL_EVENTS_DB=/opt/sentinel/data/events.db
```

Listening sockets after deploy:

```text
udp *:8001
tcp 0.0.0.0:8001
tcp 127.0.0.1:8084
tcp 127.0.0.1:4222
tcp 0.0.0.0:9090
tcp *:8000
```

Journal after restart:

```text
EventStore opened READ-ONLY at /opt/sentinel/data/events.db
sentinel-dashboard-backend WebTransport/QUIC listening port=8001
sentinel-dashboard-backend HTTPS listening http_bind=0.0.0.0:8001 bundle=/opt/sentinel/console-dist
event subscriber: subscribed to SENTINEL_EVENTS
```

No panic/error/backtrace regression was found in the post-restart journal scan.

## HTTP Route Smoke

Login used the VM-local dashboard key from `/opt/sentinel/config/dashboard-backend.env`; the key was not printed.

```text
LOGIN status=200
PUBLIC /api/cert-hash status=200
/api/agents unauth=401 auth=200 agents_len=60
/api/rooms unauth=401 auth=200 rooms_len=26
/api/rooms/buero-admin/detail unauth=401 auth=200 occupants_len=2;events_db=ok;room_id=buero-admin;id=buero-admin
/api/metrics unauth=401 auth=200 keys=kpi
/api/metrics/ebpf unauth=401 auth=200 available=True
/api/metrics/pipeline unauth=401 auth=200 providers_len=0;available=False;gateway=offline
/api/metrics/tick unauth=401 auth=200 available=True;prometheus=ok
/api/cockpit unauth=401 auth=200 incidents_len=200;events_db=ok
/api/events?limit=3 unauth=401 auth=200 events_len=3;events_db=ok
/api/events/types unauth=401 auth=200 types_len=29;events_db=ok
/api/control/status unauth=401 auth=200 gateway=offline;connected=False;paused=False
/api/control/platform-analyses?limit=3 unauth=401 auth=200 list_len=3
/api/control/snapshots unauth=401 auth=200 list_len=156
/api/control/platform-state unauth=401 auth=200 agents_len=26
```

Gateway/Judge remained inactive for the smoke. Gateway-dependent read surfaces degraded with authenticated HTTP 200 instead of crashing.

## WebTransport Topic Smoke

The ignored Rust live test was compiled through `cargo remote`. The build server can SSH to the VM, but direct UDP/QUIC to `10.0.0.240:8001` is blocked, and an SSH TCP forward is not sufficient for WebTransport. The cargo-remote-built test binary was therefore copied to the deploy VM and executed against loopback `https://127.0.0.1:8001`.

```text
/tmp/live_wt_topics-pr-a --ignored --nocapture

running 1 test
live WT topics: ["agent_live", "hello", "kpi", "room_live"]
live WT counts: agents=60 rooms=26
test live_vm_connect_snapshot_contains_room_live_and_kpi ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.07s
```

## Final VM State

```text
sentinel-dashboard-backend: active
sentinel-gateway: inactive
sentinel-judge: inactive
sentinel-health-monitor.timer: inactive
sentinel-health-monitor.service: inactive
```

The final journal scan after the WebTransport smoke found no panic, backtrace, permission, segmentation, fatal, or thread panic entries. Remaining warnings were NATS idle/no-responder messages and one `wt session error=not connected` produced by the failed TCP tunnel attempt before running the UDP/WebTransport test on the VM loopback.

## Final Redeploy Recheck

After pruning rustfmt-only noise from unchanged files, the current worktree was rechecked and redeployed.

```text
cargo remote -c -- test -p sentinel-dashboard-backend
  PASS: unit 20/20, auth_routes 5/5, live_wt_topics ignored/compiled, resilience 1/1, wt_roundtrip 3/3

cargo remote -c -- clippy --workspace --all-targets -- -D warnings
  PASS

cargo remote -c -- build -p sentinel-dashboard-backend --release
  PASS
```

Final deploy used:

```text
ExecStart=/opt/sentinel/bin/sentinel-dashboard-backend
Environment=SENTINEL_DASHBOARD_WT_BIND=0.0.0.0:8001
Environment=CORTEX_GATEWAY_PROXY_URL=http://127.0.0.1:8080
Environment=SENTINEL_DAEMON_PROMETHEUS_URL=http://127.0.0.1:9090
Environment=SENTINEL_EVENTS_DB=/opt/sentinel/data/events.db
sentinel-dashboard-backend: active
```

HTTP smoke after the final redeploy:

```text
LOGIN status=200
PUBLIC /api/cert-hash status=200
/api/agents unauth=401 auth=200 agents_len=60
/api/rooms unauth=401 auth=200 rooms_len=26
/api/rooms/buero-admin/detail unauth=401 auth=200 occupants_len=2;events_db=ok;room_id=buero-admin;id=buero-admin
/api/metrics unauth=401 auth=200 keys=kpi
/api/metrics/ebpf unauth=401 auth=200 available=True;prometheus=None
/api/metrics/pipeline unauth=401 auth=200 providers_len=0;available=False;gateway=offline
/api/metrics/tick unauth=401 auth=200 available=True;prometheus=ok
/api/cockpit unauth=401 auth=200 incidents_len=200;events_db=ok
/api/events?limit=3 unauth=401 auth=200 events_len=3;events_db=ok
/api/events/types unauth=401 auth=200 types_len=29;events_db=ok
/api/control/status unauth=401 auth=200 gateway=offline;connected=False;paused=False
/api/control/platform-analyses?limit=3 unauth=401 auth=200 list_len=3
/api/control/snapshots unauth=401 auth=200 list_len=156
/api/control/platform-state unauth=401 auth=200 agents_len=26
```

WebTransport smoke after the final redeploy:

```text
live WT topics: ["agent_live", "hello", "kpi", "room_live"]
live WT counts: agents=60 rooms=26
test live_vm_connect_snapshot_contains_room_live_and_kpi ... ok
```

Final VM state after the redeploy:

```text
sentinel-dashboard-backend: active
sentinel-gateway: inactive
sentinel-judge: inactive
sentinel-health-monitor.timer: inactive
sentinel-health-monitor.service: inactive
ExecMainPID=6848
ActiveEnterTimestamp=Tue 2026-06-02 00:07:00 UTC
```

No panic, backtrace, permission, segmentation, fatal, or thread panic entries were found since the final service start.
