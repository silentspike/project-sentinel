# Issue #464 Live Evidence

Date: 2026-06-02
Target: `ubuntu@10.0.0.240`, dashboard backend on `https://127.0.0.1:8001`

## WebTransport CAS Smoke

Command: copied `target/debug/deps/live_wt_topics-29958844d8949b71` to the VM and ran the ignored live tests against loopback.

Result:

```text
running 2 tests
live WT topics: ["agent_live", "hello", "kpi", "room_live"]
live WT counts: agents=60 rooms=26
test live_vm_connect_snapshot_contains_room_live_and_kpi ... ok
live event_log_cas stats: events=9931 max_id=11147913 blocks=9931/9931 bytes=3286965 full=4632959 dedup=0.0000 savings=0.2905
test live_vm_event_log_cas_bi_stream_reassembles_events ... ok

test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 2.28s
```

Journal evidence after browser sync showed incremental CAS deltas after the initial sync, including `sent_blocks=53`, `sent_blocks=41`, `sent_blocks=0`, and `sent_blocks=55`.

## UI Evidence

Files:

- `activity-cas-live.png`
- `activity-cas-live.json`

Observed UI metrics:

```json
{
  "activityCount": "10000 Events",
  "renderedEvents": 50,
  "agentCount": "60 / 60"
}
```

## 0-Token State

```text
backend=active
gateway=inactive
judge=inactive
health_timer=inactive
health_service=inactive
```

No panic lines were found in `sentinel-dashboard-backend` journal after the live smoke. Gateway/Prometheus degradation warnings were expected because the token-sensitive upstream services stayed inactive.
