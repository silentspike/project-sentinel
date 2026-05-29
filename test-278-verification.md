# Issue #278 Verification

Date: 2026-05-29
Repo: `/work/company/project-sentinel`
Branch: `feat/issues-278-379-nightrun-fuse`
Deploy target: `ubuntu@10.0.0.240`

## Hardware Context

- Host: `sentinel-ubuntu-2404`
- CPU: Intel Core i7-3930K @ 3.20 GHz, Sandy Bridge-E, 2011, KVM, 8 vCPU
- Build/test policy: Rust build, test, and clippy via `cargo remote -c --`.
- Runtime policy: benchmarks and live evidence on the Deploy-VM only.
- Gateway policy: real `sentinel-gateway` stayed `inactive`; a local fake gateway on `127.0.0.1:8080` was used only for the non-token success path.

## Implementation Verified

- Added async `evolution_task` with bounded workers and async `reqwest::Client`.
- Removed daemon `reqwest::blocking` usage.
- Shift-transition and Operator-API nightrun enqueue `EvolutionJob`s instead of waiting for LLM completion.
- ECS tick loop drains `EvolutionResult`s and writes voice style, behavioral notes, narrative, and `EVOLUTION_VERSION` to redb.
- `/operator/nightrun` now responds with `agents_queued`.

## Test Gates

```text
cargo fmt --check -> PASS
cargo remote -c -- test -p sentinel-fs --lib -> 94 passed
cargo remote -c -- test -p sentinel-daemon --lib -> 202 passed
cargo remote -c -- clippy --workspace --all-targets -- -D warnings -> PASS after clippy::int_plus_one fix
cargo remote -c -- build -p sentinel-daemon --release -> PASS
```

Focused tests:

```text
cargo remote -c -- test -p sentinel-daemon evolution_task --lib -> 5 passed
cargo remote -c -- test -p sentinel-daemon nightrun_request_returns_agents_queued_and_forwards_command --lib -> 1 passed
```

## VM Deploy Evidence

```text
systemctl is-active sentinel-daemon -> active
systemctl is-active sentinel-gateway -> inactive
sha256(target/release/sentinel-daemon) -> 3db188b24a308e94e9eed2ed957044fdc861da6510fc7399b27083e12a0eb9b6
journalctl -> Evolution Background-Task initialisiert
```

## Nightrun Background Evidence

Command:

```text
curl -sS -w "\nHTTP %{http_code} time_total=%{time_total}\n" \
  -X POST http://127.0.0.1:8084/operator/nightrun \
  -H "Content-Type: application/json" -d "{}"
```

Output:

```text
{"accepted":true,"agents_queued":51,"message":"Nightrun-Konsolidierung gestartet"}
HTTP 202 time_total=0.000669
```

Journal:

```text
Nightrun abgeschlossen agents=51 consolidated=109 evolution_jobs_queued=17 dry_run=false
Evolution Background-Job gestartet agent=Sandra Vogel source="nightrun"
Evolution Background-Job abgeschlossen agent=Sandra Vogel source="nightrun" voice_style=true behavioral_notes=true
Evolution nach redb geschrieben, EVOLUTION_VERSION = 29 agent=Sandra Vogel source="nightrun" version=29 voice_style=true behavioral_notes=true
Tick Checkpoint tick=1447020 sim_hour="17.77"
```

Fake gateway log:

```text
POST /v1/chat/completions HTTP/1.1" 200
```

## Shift Transition / Gateway-Failure Evidence

The fake gateway was stopped and `sentinel-gateway` remained inactive. `time_scale` was temporarily set to `600.0`, then restored to `1.0` after the test.

```text
marker=2026-05-29T05:31:57+00:00
systemctl is-active sentinel-gateway -> inactive
time_scale = 600.0
```

Journal:

```text
05:33:41.016 Tick Checkpoint tick=1447140 sim_hour="10.11"
05:33:42.202 Schichtwechsel erkannt old=3 new=1
05:33:42.264 Evolution Background-Job eingereiht agent=David Nguyen source="shift_transition"
05:33:42.266 Evolution LLM Call fehlgeschlagen agent="David Nguyen" error=error sending request for url (http://localhost:8080/v1/chat/completions)
05:33:42.266 Evolution Background-Job abgeschlossen agent=David Nguyen source="shift_transition" voice_style=false behavioral_notes=false
05:33:43.657 Schichtwechsel abgeschlossen removed=17 spawned=17 active=26
05:33:46.095 Evolution nach redb geschrieben, EVOLUTION_VERSION = 30 agent=David Nguyen source="shift_transition" version=30 voice_style=false behavioral_notes=false
```

Measured transition window:

```text
Schichtwechsel erkannt -> Schichtwechsel abgeschlossen: ~1.455 s
```

System metrics during the accelerated shift:

```text
mpstat tail: idle mostly 87-100%, peak iowait ~6.27%
iostat tail: sda peak ~50% util during redb/event writes
vmstat tail: no swap, CPU idle mostly >=87%
```

After the test:

```text
time_scale = 1.0
systemctl is-active sentinel-daemon -> active
systemctl is-active sentinel-gateway -> inactive
port 8080 -> no listener
```

## AC Evidence

| AC | Result | Evidence |
|---|---|---|
| AC-1 | PASS | Nightrun API returned in 0.669 ms; LLM work ran in `evolution_task` background logs. |
| AC-2 | TARGET MISS | Shift took ~1.455 s from detection to completion. LLM is removed from the path, but Hippocampus consolidation plus sandbox work still exceed `<1s` on this VM. |
| AC-3 | PASS | Fake gateway returned voice/notes; redb writes logged `voice_style=true behavioral_notes=true`. |
| AC-4 | PASS | `EVOLUTION_VERSION` increments are visible in nightrun and shift-transition redb write logs. |
| AC-5 | PASS | `/operator/nightrun` queued the same background path and reported `agents_queued=51`. |
| AC-6 | PASS | Gateway-down shift logged warnings, completed jobs with optional fields false, wrote redb narrative/version, and did not crash. |
| AC-7 | PASS | Remote Rust tests, fmt, clippy, and release build passed. |

## Residual Risk

The main #278 LLM-blocking defect is fixed and deployed. The strict `<1s` total shift-transition target is still unmet on the Deploy-VM; remaining optimization should target Hippocampus archive/consolidation writes and sandbox spawn/teardown.
