# Issue #382 Verification - NMDA Episode Selection

Date: 2026-05-29

Scope: finalize NMDA episode-selection threshold policy, expose selection-quality metrics, and prove deterministic replay for a real Night-Run path on the Deploy-VM.

## Environment

Runtime host:

```text
ubuntu@10.0.0.240
Model name: Intel(R) Core(TM) i7-3930K CPU @ 3.20GHz
CPU(s): 8
Thread(s) per core: 1
Core(s) per socket: 8
Socket(s): 1
Linux sentinel-ubuntu-2404 6.17.0-22-generic
```

Service state before and after the final measurement:

```text
sentinel-daemon: active
sentinel-projection: active
sentinel-gateway: inactive
time_scale = 1.0
```

Evidence directory on the VM:

```text
/tmp/issue-382-20260529T112955Z-controlled-actions
```

The final measurement used controlled `AgentActionReceived` events inserted into the VM event store because the organic pending episode queue had already been drained by earlier Night-Run attempts. The real daemon `EpisodeProducer` consumed those events, and the real operator Night-Run consolidated them. No cortex/gateway token path was started.

## AC Matrix

| AC | Requirement | Result | Evidence |
|---|---|---|---|
| AC-1 | Score thresholds documented and reasoned, not "tuning" | PASS | `crates/sentinel-hippocampus/src/selection.rs` defines the calibrated profile: threshold `0.25`, narrative threshold `0.25`, max episodes `10`, and rationale. Boundary tests cover accept/reject cases. |
| AC-2 | Real Night-Run on Deploy-VM measures X/Y episodes consolidated and plausibility | PASS | Operator Night-Run `operator-tick-1467901-shift-1-to-1`: `2/3` episodes consolidated, selection rate `0.666666666666667`, score range `0.00491937687892867..0.550970210440011`, threshold `0.25`. |
| AC-3 | Deterministic replay of Night-Run gives identical hash-chain | PASS | Replay loaded 5 events, replayed 4, and matched hash `2b41609c53b17955beb7bbfd1656b1f5aeac2e996933926d516a001d7fc23751`. |
| AC-4 | Gap-doc Cluster 02 updated to done with evidence | PASS | `docs/togaf-gap-v22.md` Cluster 02 Memory row now references #382 and this verification file. |

## AC-1 Threshold Policy

Code evidence:

```text
NMDA_CONSOLIDATION_THRESHOLD = 0.25
NMDA_NARRATIVE_INCLUSION_THRESHOLD = 0.25
NMDA_MAX_CONSOLIDATION_EPISODES = 10
NMDA_SELECTION_RATIONALE = "0.25 keeps relevant work episodes while rejecting medium-context and routine noise"
```

The threshold rationale is based on the score formula:

```text
score = relevance * emotion * repetitions * (1 / (1 + hours_ago))
```

Boundary coverage:

```text
relevant work episode: 0.8 * 0.7 * 1 * 0.5 = 0.28 -> consolidate
medium-context episode: 0.5 * 0.5 * 1 * 0.5 = 0.125 -> archive-only
routine noise: 0.1 * 0.05 * 1 * 0.5 = 0.0025 -> archive-only
```

## AC-2 VM Night-Run

Controlled events consumed by the real daemon `EpisodeProducer`:

```text
fixture_correlation_id=issue-382-controlled-20260529T112955Z
inserted_max_event_id=10725615
episode_producer_offset_seen_after_seconds=22
episode_producer_offset=10725668
```

Seeded events:

```text
10725613 AGENT-01 talk "konflikt deadline fehler escalation issue 382 controlled evidence"
10725614 AGENT-02 talk "meeting konflikt problem deadline issue 382 controlled evidence"
10725615 AGENT-03 move "routine hallway movement issue 382 low relevance control"
```

Operator trigger:

```bash
curl -sS -X POST http://127.0.0.1:8084/operator/nightrun \
  -H 'Content-Type: application/json' \
  -d '{"dry_run":false}'
```

Response:

```json
{"accepted":true,"agents_queued":51,"message":"Nightrun-Konsolidierung gestartet"}
```

Completed event:

```text
new_nightrun_completed_id=10725673
run_id=operator-tick-1467901-shift-1-to-1
hash_chain=2b41609c53b17955beb7bbfd1656b1f5aeac2e996933926d516a001d7fc23751
total_episodes=3
consolidated=2
selection_rate=0.666666666666667
threshold=0.25
max_per_agent=10
score_min=0.00491937687892867
score_avg=0.36895326591965
score_max=0.550970210440011
duration_ms=196
```

Per-agent selection:

```text
AGENT-01 Thomas Mueller processed=1 consolidated=1 duration_ms=19
AGENT-02 Lisa Brenner   processed=1 consolidated=1 duration_ms=8
AGENT-03 Max Richter    processed=1 consolidated=0 duration_ms=8
```

Plausibility:

```text
Thomas/Lisa talk+conflict+deadline episodes scored ~0.551, above threshold 0.25, so they were consolidated.
Max routine movement scored ~0.0049, below threshold 0.25, so it stayed archive-only.
```

System metrics captured during the VM run:

```text
vmstat_samples=23 avg_idle=99.48 avg_cpu_used=0.52
mpstat_samples=22 avg_idle=99.34 avg_cpu_used=0.66
iostat_device_samples=91 avg_util_pct=24.55
```

## AC-3 Replay

Replay command:

```bash
cd /opt/sentinel
/opt/sentinel/bin/sentinel-nightrun \
  --config /opt/sentinel/config/nightrun.toml \
  --replay-run-id operator-tick-1467901-shift-1-to-1 \
  --expected-hash 2b41609c53b17955beb7bbfd1656b1f5aeac2e996933926d516a001d7fc23751 \
  --json
```

Replay output:

```json
{
  "mode": "Replay",
  "result": {
    "run_id": "operator-tick-1467901-shift-1-to-1",
    "events_loaded": 5,
    "events_replayed": 4,
    "hash_chain_valid": true,
    "final_hash": "2b41609c53b17955beb7bbfd1656b1f5aeac2e996933926d516a001d7fc23751",
    "expected_hash": "2b41609c53b17955beb7bbfd1656b1f5aeac2e996933926d516a001d7fc23751"
  }
}
```

## Test Gates

Commands run before the VM deployment:

```bash
cargo fmt --check
git diff --check
cargo remote -c -- test -p sentinel-hippocampus --lib
cargo remote -c -- test -p sentinel-hippocampus --test acceptance
cargo remote -c -- test -p sentinel-common --lib
cargo remote -c -- test -p sentinel-nightrun --lib
cargo remote -c -- test -p sentinel-nightrun --test integration
cargo remote -c -- test -p sentinel-daemon --lib
cargo remote -c -- clippy -p sentinel-hippocampus --all-targets -- -D warnings
cargo remote -c -- clippy -p sentinel-common --all-targets -- -D warnings
cargo remote -c -- clippy -p sentinel-nightrun --all-targets -- -D warnings
cargo remote -c -- clippy -p sentinel-daemon --all-targets -- -D warnings
```

Results:

```text
sentinel-hippocampus --lib: 106 passed / 0 failed
sentinel-hippocampus acceptance: 4 passed / 0 failed
sentinel-common --lib: 52 passed / 0 failed
sentinel-nightrun --lib: 37 passed / 0 failed
sentinel-nightrun integration: 14 passed / 0 failed
sentinel-daemon --lib: 204 passed / 0 failed
all listed clippy gates: PASS
cargo fmt --check: PASS
git diff --check: PASS
```

Release build and deployed VM hashes:

```text
316344ee37ccbc33374058f5d08e86919bb6aa336a472d4c381234dc61e4c174  /opt/sentinel/bin/sentinel-daemon
6c8ce99acb86cb00fa23152117171dcb11c5fbbe25b31fa7b9076ab9da73482d  /opt/sentinel/bin/sentinel-nightrun
```

## Notes

- MainRag was unavailable during the run: `localhost:3001` connection refused.
- The first post-deploy payload smoke run produced a valid `0/0` Night-Run with replay, but it was not used as AC-2 evidence because the queue was empty.
- The final AC-2 evidence is the controlled-action VM run above: `2/3` consolidated with threshold/score proof and hash replay.
