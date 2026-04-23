# PROGRESS

## Status

- Plan source: `/work/company/codex-plan279.md`
- Overall status: `TASK_8_DONE_TASK_9_PENDING`
- Current task: `Task 9 - Slice G: Conditional FUSE/Landlock Runtime Restore`
- Current branch: `feat/issue-279-daemon-hardening-v2`
- Worktree: `/work/company/project-sentinel-279-review`
- Base: `origin/main @ 83ab01c8835804fd951e619aa048324b7ff76ddf`
- Hook status: `PreToolUse TaskUpdate + PostToolUse start-enforcer projektlokal registriert`
- Last refresh: `2026-04-23 Europe/Vienna`

## Current findings

- `$start` wurde fuer diese Ausfuehrung aktiviert.
- Projektregeln, globale Regeln, Workspace-Handover und der komplette `#279`-Plan wurden in dieser Session frisch gelesen.
- Der neue Worktree ist sauber von `origin/main` geschnitten; alte #264/#279-Stack-Worktrees bleiben unangetastet.
- `gh issue view 279` ist erreichbar; GitHub ist aktuell die SSOT.
- `#279` ist `OPEN` und steht aktuell auf `status:in-progress`.
- Der GitHub-Issue-Body war stale und wurde in Phase 0 truth-repaired; aktuelle SSOT ist `#279` mit `status:in-progress`.
- `mainrag search "issue 279 daemon hardening runtime" --source claude-conversations --limit 5` ist aktuell nicht nutzbar, weil `localhost:3001` `Connection refused` liefert. Das ist kein Task-Blocker, aber ein dokumentierter Kontextverlust.
- Phase 0 ist erledigt:
  - frische Pre-Reset-VM-Baseline ist in `test-279-verification.md` dokumentiert
  - GitHub-Issue `#279` wurde auf frische 2026-04-23-Wahrheit repariert
  - canonical `main` wurde remote gebaut und auf `10.0.0.240` deployed
  - Post-Reset-Baseline zeigt erwartetes `runtime_health_http=404`
  - Drift bleibt auf `main` reproduzierbar: API/Projection `57`, live cgroups `29`
- Review-Entscheidungen aus dem Plan:
  - Haiku-/Model-Policy ist out-of-scope fuer `#279`.
  - Der Branch darf keine #264-Stack-Commits enthalten.
  - Frische VM-Zahlen muessen dynamisch gemessen werden, nicht aus alten Reviews kopiert werden.
  - Canonical-main-Reset muss ohne `/operator/runtime-health` messbar sein, weil `main` diesen Endpoint noch nicht hat.
  - Operator-Auth wird spaeter ueber einen gemeinsamen `/tmp/opcurl` Helper operationalisiert.
- FUSE/Landlock bekommt nur bei aktivem Slice G eigene Build-/Deploy-Gates.
- Slice A ist erledigt:
  - `GET /operator/runtime-health` ist als read-only Operator-Pfad verdrahtet
  - Snapshot deckt Runtime, Projection, Cgroups, Security-Runtime und Worker-State ab
  - Operator-Auth-Schutz fuer den Endpoint ist im Unit-Test belegt
  - initialer Warnungsfund `unused import RuntimeHealthSnapshot` wurde vor Commit bereinigt
- Slice B ist erledigt:
  - `POST /operator/runtime/reconcile` ist als Runtime-Control-Pfad verdrahtet
  - Reconcile entfernt unerwartete Runtime-Fragmente, orphan Cgroups und stale Security-Snapshots
  - Expected active Agents koennen mit Backoff N=3 respawned werden
  - Projection-Rebuild wird ueber `.projection-rebuild-request` entkoppelt angefordert
  - aktuelle #264-SIGSTOP-Semantik wurde bei Konfliktauflösung beibehalten
- Slice C ist erledigt:
  - `POST /operator/runtime/stall-restart-test` ist als synchroner Operator-Testhook verdrahtet
  - `PlatformSideEffect::RestartAgent` nutzt jetzt einen same-tick Fast-Respawn statt verzögertem Despawn
  - der Fast-Restart entfernt alte Runtime-, Security- und Sandbox-Fragmente und respawned danach denselben Agent
  - Remote-Tests, FUSE-Release-Build und Artifact-Hash sind dokumentiert
- Slice D ist erledigt:
  - `service_health` ist per `catch_unwind` in-process supervised
  - `POST /operator/runtime/panic-test` triggert kontrolliert den Service-Health-Panic-Test ueber den ECS-Thread
  - Runtime-Health-Worker-State zeigt `running`, `restart_count`, `last_error` und `thread_name`
  - Remote-Tests, FUSE-Release-Build und Artifact-Hash sind dokumentiert
- Slice E ist erledigt:
  - Platform-Controlplane-Triggerqueue ist bounded und coalesced doppelte Trigger
  - LLM-Analyzer-Channel ist bounded und zaehlt Dropped-Requests
  - Runtime-Health publiziert Queue-Depth, Dropped- und Coalesced-Counter
  - `POST /operator/runtime/analysis-flood-test` erzeugt deterministische Queue-Pressure fuer VM-/AC-Evidence
  - Remote-Tests, FUSE-Release-Build, Clippy, Scope-Guard und Artifact-Hash sind dokumentiert
- Slice F ist erledigt:
  - Projection-Worker konsumiert `.projection-rebuild-request` und fuehrt Full-Rebuild in-place aus
  - Projection-Store bietet read-only Open fuer Runtime-Health ohne Migration/Startup-Cleanup
  - Runtime-Health erkennt Projection-Drift und zaehlt driftende Projection-Agenten
  - Runtime-Reconcile fordert Projection-Rebuilds per Request-Datei an und vermeidet Restart-Storms, wenn `sentinel-projection` bereits aktiv ist
  - Remote-Tests, Clippy, Release-Builds und Artifact-Hashes fuer Daemon und Projection sind dokumentiert

## Blocked items

- Kein harter technischer Blocker beim Setup.
- `mainrag` ist lokal nicht verfuegbar; fuer `#279` nicht blockierend.
- Kein Phase-0-Blocker mehr.
- Der Main-Reset bestaetigt, dass der volle #279-Recovery-Scope notwendig bleibt.

## Commit references

- `dca25ac` Task [1] Phase 0 - Issue-Repair und Baseline-Reset
- `e2e7523` Task [2] Phase 1 - Donor-Audit und Clean Port
- `f5be73b` Task [3] Slice A - Runtime-Health-Read-Model
- `ca44759` Task [4] Slice B - Runtime-Control / Reconcile
- `f5e3dd9` Task [5] Slice C - Fast Stall Recovery
- `b3b7786` Task [6] Slice D - Worker-Supervision
- `bc57b4c` Task [7] Slice E - bounded Analysis-/Recovery-Pfade
- `TBD` Task [8] Slice F - Projection/API-Konvergenz
- `TBD` Task [9] Slice G - Conditional FUSE/Landlock Runtime Restore
- `TBD` Task [10] Out-of-scope Follow-up - Haiku-Policy
- `TBD` Task [11] Phase 3 - Tests, Clippy, Builds
- `TBD` Task [12] Phase 4 - Deploy auf die VM
- `TBD` Task [13] Phase 5 - AC-Matrix
- `TBD` Task [14] Benchmarks
- `TBD` Task [15] PR- und Close-Sequenz
- `TBD` Task [16] Plan-Verifikation

## Task table

| # | Task | Status | Scope | Evidence |
|---|------|--------|-------|----------|
| 1 | Phase 0 - Issue-Repair und Baseline-Reset | DONE | frische VM-Baseline, Issue-Body-Repair, canonical-main-Reset, Post-Reset-Baseline ohne runtime-health | command, system, inspect |
| 2 | Phase 1 - Donor-Audit und Clean Port | DONE | alte #279-Donor-Commits pruefen, nur scope-konforme Teile uebernehmen, Haiku ausschliessen | inspect, command |
| 3 | Slice A - Runtime-Health-Read-Model | DONE | `/operator/runtime-health`, Snapshot-Modell, Auth/Loopback, Tests | inspect, command, system |
| 4 | Slice B - Runtime-Control / Reconcile | DONE | `/operator/runtime/reconcile`, stale/orphan/zombie cleanup, bounded retries | inspect, command, system |
| 5 | Slice C - Fast Stall Recovery | DONE | deterministischer Stall-Testhook, same-tick Fast-Respawn, Runtime/Security/Sandbox-Recreate | inspect, command, system |
| 6 | Slice D - Worker-Supervision | DONE | catch_unwind + in-process worker respawn, panic-test Hook | inspect, command, system |
| 7 | Slice E - bounded Analysis-/Recovery-Pfade | DONE | bounded trigger queues, coalescing/drop counters, flood-test | inspect, command, system |
| 8 | Slice F - Projection/API-Konvergenz | DONE | read-only Projection-Reads, Rebuild-Request, drift-heal, no restart storm | inspect, command, system |
| 9 | Slice G - Conditional FUSE/Landlock Runtime Restore | PENDING | nur falls Main-Repro es braucht: FUSE/Landlock Runtime Restore, eigene Build-/Deploy-Gates | inspect, command, system |
| 10 | Out-of-scope Follow-up - Haiku-Policy | PENDING | separates Gateway-/Policy-Issue oder Kommentar, nicht #279-Close-Bedingung | command, inspect |
| 11 | Phase 3 - Tests, Clippy, Builds | PENDING | cargo remote only, relevant packages, conditional FUSE/Landlock matrix | command |
| 12 | Phase 4 - Deploy auf die VM | PENDING | ExecStart pruefen, scp/install, restart, post-restart smoke | command, system |
| 13 | Phase 5 - AC-Matrix | PENDING | alle ACs mit Command, Output, PASS auf VM | command, system |
| 14 | Benchmarks | PENDING | Recovery, Queue, Soak, Sidecar-Monitoring | command, system |
| 15 | PR- und Close-Sequenz | PENDING | PR mit Pflichtsektionen, labels, merge, branch delete, issue close erst nach verified | command |
| 16 | Plan-Verifikation | PENDING | Plan Zeile fuer Zeile gegen Ergebnis pruefen, Abweichungen fixen | inspect, command |

## Task 1 - Phase 0: Issue-Repair und Baseline-Reset

### Pre-task self-check

- Was muss getan werden: frische Runtime-Wahrheit erfassen, GitHub-Issue korrigieren, canonical `main` auf der VM deployen, danach Baseline ohne `/operator/runtime-health` belegen.
- Welche ACs muessen hier passen:
  - AC-1: frische experimentelle VM-Baseline ist dokumentiert.
  - AC-2: `#279` Issue-Body ist truth-repaired und enthaelt keine stale Blocked-by-/Zahlen-Behauptung.
  - AC-3: canonical `main` Binaries fuer Daemon und Projection sind remote gebaut, Deploy-Pfad inklusive ExecStart ist belegt.
  - AC-4: Post-Reset-Baseline ohne `/operator/runtime-health` ist belegt.
- Wie wird bewiesen: `gh`, `ssh ubuntu@10.0.0.240`, `cargo remote -c --`, `scp`, `systemctl`, `curl`, `sqlite3`, `journalctl`.
- Erwartete Dateien: `PROGRESS.md`, `docs/issue-279-phase0-baseline.md`, optional temporaere Issue-Body-Datei.
- Risiken:
  - VM laeuft aktuell moeglicherweise mit experimentellen #279-Binaries; Phase 0 ersetzt diese bewusst durch canonical `main`.
  - Nach Main-Reset ist `/operator/runtime-health` erwartbar nicht vorhanden.
  - Deployment darf nicht gegen falsche systemd `ExecStart`-Pfade kopieren.

### Outcome

- Fresh pre-reset VM baseline captured:
  - services active
  - API agents `57`
  - Projection agents `57`
  - experimental runtime-health expected active agents `26`
  - experimental runtime-health runtime agents `26`
  - experimental runtime-health orphan cgroups `33`
  - experimental runtime-health projection drift `true`
- GitHub issue `#279` body repaired at `2026-04-23T09:47:19Z`:
  - removed stale `Blocked by #278`
  - removed old 2026-04-21 numbers as current truth
  - removed #264-stacked branch assumption
  - kept Haiku/model policy out-of-scope
  - kept strict `status:verified` close rule
- Canonical `main` artifacts built through `cargo remote -c --`:
  - `sentinel-daemon --features fuse`: `Finished release profile [optimized] target(s) in 12m 04s`
  - `sentinel-projection-service`: `Finished release profile [optimized] target(s) in 1m 37s`
- Main artifacts deployed to exact VM `ExecStart` paths:
  - `/opt/sentinel/bin/sentinel-daemon`
  - `/opt/sentinel/bin/sentinel-projection`
  - timestamp backup created with `20260423T100300Z`
- Post-reset baseline without runtime-health captured:
  - services active
  - API agents `57`
  - Projection agents `57`
  - cgroup dirs `59`
  - live cgroup process dirs `29`
  - `/operator/runtime-health` returns `404`, expected on current `main`
- Conclusion:
  - canonical `main` does not self-heal the runtime/projection/cgroup drift
  - full #279 recovery scope remains required

### Evidence

- `test-279-verification.md` contains Phase-0 command/output evidence.
- AC-1 PASS: Pre-reset VM baseline captured on `10.0.0.240`.
- AC-2 PASS: `gh issue edit 279 --repo silentspike/project-sentinel --body-file docs/issue-279-body.md`; follow-up `gh issue view` confirmed repaired body.
- AC-3 PASS: remote release builds succeeded and deployed SHA256 values match installed VM binaries:
  - `2b31820c751c6f3dd0eb8e1282e827d64c5b2d9b016bf56fcede4c765ee86883  /opt/sentinel/bin/sentinel-daemon`
  - `10b845881d24ed0c661ba5a18de4ebddad44d404d15f0231ef1ce83a83855ce3  /opt/sentinel/bin/sentinel-projection`
- AC-4 PASS: Post-reset baseline uses only main-compatible checks and confirms `runtime_health_http=404`.

## Task 2 - Phase 1: Donor-Audit und Clean Port

### Pre-task self-check

- Was muss getan werden: alten Donor-Branch lesen, Commit-/Diff-Scope klassifizieren, Haiku- und #264-fremde Arbeit ausschliessen, eine saubere Port-Reihenfolge fuer die Code-Slices herstellen.
- Welche ACs muessen hier passen:
  - AC-1: Donor-Commits sind gegen `main` sichtbar und klassifiziert.
  - AC-2: Out-of-scope Commits/Dateien sind explizit ausgeschlossen.
  - AC-3: Clean-port Reihenfolge fuer Slice A-F und conditional Slice G ist dokumentiert.
- Wie wird bewiesen: `git log`, `git diff --name-status`, gezielte Code-Inspection, Abgleich gegen `codex-plan279.md`.
- Erwartete Dateien: `PROGRESS.md`, ggf. `test-279-verification.md`; noch kein Produktionscode, solange nur Audit laeuft.
- Risiken:
  - Der alte Donor-Branch ist auf #264 gestackt und darf nicht direkt gemergt werden.
  - Haiku-Pinning ist bewusst out-of-scope fuer #279.

### Outcome

- Donor sources inspected:
  - old donor: `/work/company/project-sentinel-issue279`, branch `feat/issue-279-daemon-hardening`
  - safer stack donor: `/work/company/project-sentinel-issue279-stack`, branch `feat/issue-279-daemon-hardening-stack`
- The old donor contains `dbb1b8e` which pins agent LLM requests to `haiku`; this is rejected for #279.
- The stack donor removed the Haiku commit, but still spans a broad stacked diff and is not mergeable as a branch.
- Clean-port source of truth:
  - use stack donor commits as code references
  - path-limit each slice
  - do not port stale donor evidence or donor `PROGRESS.md`
- Port order is fixed:
  - Slice A from `29a0fb5`
  - Slice B from `146ef43`
  - Slice C from `fdc40c7`
  - Slice D from `6919e66`
  - Slice E from `389b5e6`
  - Slice F from `14560c4` plus relevant stabilization from `b1e376b`
  - Slice G from `6c31975` only if needed
  - Benchmarks from `ffca58b` with fresh current evidence

### Evidence

- `test-279-verification.md` contains Phase-1 command/output evidence.
- AC-1 PASS:
  - `git log --oneline --decorate --reverse origin/main..HEAD` run on both donor worktrees.
  - `git range-diff origin/main...feat/issue-279-daemon-hardening-stack` shows the old #264 lineage and the #279 commit sequence.
- AC-2 PASS:
  - old donor `dbb1b8e` / `services/sentinel-daemon/src/llm_bridge.rs` is explicitly rejected.
  - stale donor `PROGRESS.md`, old evidence and branch-wide #264 paths are excluded.
- AC-3 PASS:
  - clean port order and path-limited rules are documented in `test-279-verification.md`.

## Task 3 - Slice A: Runtime-Health-Read-Model

### Pre-task self-check

- Was muss getan werden: Runtime-Health-Snapshot aus dem Donor path-limited portieren und `GET /operator/runtime-health` als loopback/auth-konformen Read-Pfad verfuegbar machen.
- Welche ACs muessen hier passen:
  - AC-1: `runtime_health.rs` existiert und berechnet runtime/cgroup/projection/worker truth ohne Seiteneffekte.
  - AC-2: Operator-Endpoint ist verdrahtet und nutzt die bestehende Operator-Auth-/Loopback-Struktur.
  - AC-3: relevante Remote-Rust-Tests oder mindestens package build fuer Slice A sind gruen.
- Wie wird bewiesen: Donor-Diff-Inspection, Code-Diff, `cargo remote -c -- test/build`.
- Erwartete Dateien: `services/sentinel-daemon/src/runtime_health.rs`, `lib.rs`, `operator_api.rs`, `orchestrator.rs`, `service_health.rs`, ggf. `Cargo.toml/Cargo.lock`.
- Risiken:
  - Donor-Patch darf keine Reconcile-/Stall-/Queue-Semantik vorziehen.
  - `runtime-health` muss read-only bleiben.

### Outcome

- Path-limited donor Slice A was applied from stack donor `29a0fb5`.
- Merge conflict in `services/sentinel-daemon/src/operator_api.rs` was resolved by keeping current `main`'s `open_fs_layer` security/FUSE helper and adding only the new `is_protected_read_path()` read-auth helper.
- Added `services/sentinel-daemon/src/runtime_health.rs`.
- Added `sentinel-projection` as `sentinel-daemon` dependency so the daemon can read Projection truth without mutating Projection state.
- Wired `SharedRuntimeHealthState` through `orchestrator.rs` into the ECS tick loop and Operator API.
- Added `GET /operator/runtime-health` as a protected read path; it returns the current snapshot and respects the existing operator shared-secret auth rule.
- Extended `ServiceHealthChecker` with a read-only worker snapshot for runtime-health reporting.
- Fixed the initial `unused import RuntimeHealthSnapshot` warning by moving the import into the test module.

### Evidence

- `cargo remote -c -- test -p sentinel-daemon runtime_health -- --nocapture`
  - PASS: `3 passed; 0 failed; 173 filtered out; finished in 10.28s`
  - Covered tests:
    - `runtime_health::tests::build_snapshot_marks_missing_projection_and_security_as_stale`
    - `operator_api::tests::runtime_health_endpoint_returns_snapshot`
    - `operator_api::tests::runtime_health_endpoint_requires_auth_when_secret_is_set`
- `cargo remote -c -- build -p sentinel-daemon --release --features fuse`
  - PASS: `Finished release profile [optimized] target(s) in 1m 07s`
  - artifact hash: `7b5145a77da0a55a3e9e9da00b2d9036ce943df748d327b4095f87955b0034d2  target/release/sentinel-daemon`
- `git diff --check`
  - PASS: no whitespace/conflict errors.

## Task 4 - Slice B: Runtime-Control / Reconcile

### Pre-task self-check

- Was muss getan werden: Runtime-Control aus dem Donor path-limited portieren und `POST /operator/runtime/reconcile` als kontrollierten Reparaturpfad in den ECS-Thread verdrahten.
- Welche ACs muessen hier passen:
  - AC-1: `runtime_control.rs` definiert Request/Response, Command und bounded Respawn-Backoff.
  - AC-2: Operator-Endpoint dispatcht in den ECS-Thread und wartet bounded auf Response.
  - AC-3: Orchestrator kann stale Runtime-Fragmente, Security-Snapshots und orphan Cgroups bereinigen.
  - AC-4: Missing expected Agents koennen mit Backoff respawned oder als `repair_blocked` dokumentiert werden.
  - AC-5: Projection-Rebuild wird entkoppelt per Request-Datei angefordert.
  - AC-6: relevante Remote-Rust-Tests und Release-Build sind gruen.
- Wie wird bewiesen: Donor-Diff-Inspection, Code-Diff, `cargo remote -c -- test/build`.
- Erwartete Dateien: `services/sentinel-daemon/src/runtime_control.rs`, `lib.rs`, `operator_api.rs`, `orchestrator.rs`.
- Risiken:
  - Donor-Patch darf keine alte #264-SIGSTOP-Abschwaechung einschleppen.
  - Reconcile darf nicht inline Projection mutieren.
  - Respawn-Fehler muessen bounded bleiben und duerfen keinen Endlos-Loop erzeugen.

### Outcome

- Path-limited donor Slice B was applied from stack donor `146ef43`.
- Added `services/sentinel-daemon/src/runtime_control.rs` with:
  - `RuntimeReconcileRequest`
  - `RuntimeReconcileResponse`
  - `RuntimeControlCommand`
  - `RespawnBackoffTracker::new(3)` semantics
  - `write_projection_rebuild_request()`
- Added `POST /operator/runtime/reconcile` and a bounded `recv_timeout(10s)` response path.
- Wired `runtime_tx/runtime_rx` through Operator API and ECS loop.
- Added `run_runtime_reconcile()` in the orchestrator:
  - removes unexpected active runtime fragments
  - removes stale security runtime snapshots
  - tears down orphan cgroups without live PIDs
  - restores missing security runtime snapshots when core runtime is healthy
  - respawns expected active Agents when requested
  - records `repair_blocked` via `PlatformIntervention`
  - updates `SharedRuntimeHealthState` with counters and per-agent repair status
- Conflict resolution:
  - kept current `suspend_pids()` implementation with 2s multi-PID verification from #264/main lineage
  - rejected donor's narrower 250ms tracked-PID-only check

### Evidence

- `cargo remote -c -- test -p sentinel-daemon runtime_control -- --nocapture`
  - PASS: `2 passed; 0 failed; 177 filtered out`
  - Covered tests:
    - `runtime_control::tests::respawn_backoff_tracker_applies_exponential_backoff_until_blocked`
    - `runtime_control::tests::write_projection_rebuild_request_persists_request_file`
- `cargo remote -c -- test -p sentinel-daemon runtime_reconcile -- --nocapture`
  - PASS: `1 passed; 0 failed; 178 filtered out; finished in 0.61s`
  - Covered test:
    - `operator_api::tests::runtime_reconcile_is_forwarded_and_returns_response`
- `cargo remote -c -- build -p sentinel-daemon --release --features fuse`
  - PASS: `Finished release profile [optimized] target(s) in 1m 00s`
  - artifact hash: `19ce8b228b5588142399a4f712afd4cd89b1caca2be43c1273355a3ad30fe724  target/release/sentinel-daemon`

## Task 5 - Slice C: Fast Stall Recovery

### Pre-task self-check

- Was muss getan werden: Fast-Stall-Recovery aus dem Donor path-limited portieren, einen deterministischen Operator-Testhook bereitstellen und `PlatformSideEffect::RestartAgent` vom verzögerten Despawn auf same-tick Fast-Respawn umstellen.
- Welche ACs muessen hier passen:
  - AC-1: Operator-Testhook nimmt `agent_id`, `mode` und `stall_secs` an, validiert Eingaben und dispatcht in den ECS-Thread.
  - AC-2: Fast-Restart entfernt alte Runtime-, Sandbox-, eBPF- und Security-Fragmente und erzeugt danach Runtime- und Security-State neu.
  - AC-3: `PlatformSideEffect::RestartAgent` nutzt den Fast-Restart-Pfad statt nur zu despawnen.
  - AC-4: relevante Remote-Tests, FUSE-Release-Build und `git diff --check` sind gruen.
- Wie wird bewiesen: Donor-Diff-Inspection, Code-Diff, `cargo remote -c -- fmt/test/build`, `sha256sum`, `git diff --check`.
- Erwartete Dateien: `services/sentinel-daemon/src/runtime_control.rs`, `operator_api.rs`, `orchestrator.rs`, `runtime_health.rs`, `CHANGELOG.md`.
- Risiken:
  - Testhook darf kein Auth-Bypass sein; er laeuft ueber die bestehende Operator-API-Schutzschicht.
  - Stall-Test darf den ECS-Thread nicht kuenstlich schlafen lassen.
  - Fast-Restart darf keine alten cgroups oder Security-Snapshots zuruecklassen.

### Outcome

- Path-limited donor Slice C was applied from stack donor `fdc40c7`.
- Added `RuntimeControlCommand::StallRestartTest` plus typed request/response structs.
- Added `POST /operator/runtime/stall-restart-test`:
  - rejects `agent_id=0`
  - allows only `mode=sigstop` or `mode=direct`
  - rejects `stall_secs=0`
  - dispatches to the ECS owner thread and waits with `recv_timeout(10s)`
- Added `restart_agent_fast_path()` in the orchestrator:
  - captures `pid_before`
  - tears down sandbox handle and unregisters eBPF cgroup id
  - removes and terminates the old tracked process
  - removes stale security runtime state
  - despawns ECS/runtime state
  - respawns the same configured agent immediately
  - captures `pid_after` and runtime/security presence
- Replaced `PlatformSideEffect::RestartAgent` with the same fast-path restart.
- No Haiku/model-policy changes were introduced.

### Evidence

- `cargo remote -c -- fmt --check`
  - PASS after applying remote rustfmt diffs locally.
- `cargo remote -c -- test -p sentinel-daemon runtime_stall_restart -- --nocapture`
  - PASS: `2 passed; 0 failed; 180 filtered out; finished in 1.49s`
  - Covered tests:
    - `operator_api::tests::runtime_stall_restart_test_is_forwarded_and_returns_response`
    - `operator_api::tests::runtime_stall_restart_test_rejects_invalid_mode`
- `cargo remote -c -- test -p sentinel-daemon restart_agent_fast_path -- --nocapture`
  - PASS: `1 passed; 0 failed; 181 filtered out; finished in 0.14s`
  - Covered test:
    - `orchestrator::tests::test_restart_agent_fast_path_recreates_runtime_and_security_state`
  - Note: build-server test sandbox logged `bwrap: Can't find source path /work/company`; this did not fail the test and is limited to the remote test fixture path.
- `cargo remote -c -- build -p sentinel-daemon --release --features fuse`
  - PASS: `Finished release profile [optimized] target(s) in 56.20s`
  - artifact hash: `3470152e8b897217e2d2e547549b8f6759b3e733ec53d8a3ea8e00745da8d2bd  target/release/sentinel-daemon`
- `git diff --check`
  - PASS: no whitespace/conflict errors.

## Task 6 - Slice D: Worker-Supervision

### Pre-task self-check

- Was muss getan werden: Worker-Supervision aus Donor `6919e66` path-limited portieren, `service_health` per `catch_unwind` im selben Daemon-Prozess neu starten und einen deterministischen Panic-Testhook bereitstellen.
- Welche ACs muessen hier passen:
  - AC-1: `service_health`-Worker-Panics werden gefangen und erhoehen `restart_count`, ohne den Daemon-Prozess zu beenden.
  - AC-2: `POST /operator/runtime/panic-test` akzeptiert nur den erlaubten Worker `service_health`, dispatcht in den ECS-Thread und liefert eine bounded Response.
  - AC-3: Runtime-Health-Worker-State bleibt beobachtbar: `running`, `restart_count`, `last_error`, `thread_name`.
  - AC-4: Remote-Tests, FUSE-Release-Build, Artefakt-Hash und `git diff --check` sind gruen.
- Wie wird bewiesen: Code-Diff, `cargo remote -c -- fmt/test/build`, `sha256sum`, `git diff --check`.
- Erwartete Dateien: `services/sentinel-daemon/src/service_health.rs`, `runtime_control.rs`, `operator_api.rs`, `orchestrator.rs`, `CHANGELOG.md`.
- Risiken:
  - Der Panic-Test muss ein kontrollierter Testpfad bleiben und darf keinen Daemon-Crash als PASS werten.
  - Die bestehende Operator-Auth-/Loopback-Schutzschicht darf nicht umgangen werden.

### Outcome

- Path-limited donor Slice D was applied from stack donor `6919e66`.
- `ServiceHealthChecker::spawn()` supervisiert den Worker jetzt in einer Schleife mit `panic::catch_unwind(AssertUnwindSafe(...))`.
- Ein kontrollierter Panic erhoeht `restart_count`, schreibt `last_error` und startet `service-health-checker` im selben Daemon-Prozess erneut.
- `ServiceHealthControl::{PanicTest, Shutdown}` steuert Test und sauberen Drop des Workers.
- `POST /operator/runtime/panic-test` ist verdrahtet:
  - rejects empty worker
  - allows only `worker=service_health`
  - dispatches via `RuntimeControlCommand::PanicTest`
  - waits with `recv_timeout(10s)`
- Der Orchestrator behandelt den Panic-Test im ECS-Owner-Thread und ruft `service_health_checker.trigger_panic_test()`.
- No Haiku/model-policy changes were introduced.

### Evidence

- `cargo remote -c -- fmt --check`
  - PASS: no remaining rustfmt diff.
- `cargo remote -c -- test -p sentinel-daemon runtime_panic_test -- --nocapture`
  - PASS: `2 passed; 0 failed; 183 filtered out; finished in 0.07s`
  - Covered tests:
    - `operator_api::tests::runtime_panic_test_is_forwarded_and_returns_response`
    - `operator_api::tests::runtime_panic_test_rejects_invalid_worker`
- `cargo remote -c -- test -p sentinel-daemon panic_test_restarts_worker_in_process -- --nocapture`
  - PASS: `1 passed; 0 failed; 184 filtered out; finished in 0.25s`
  - Note: the printed panic/backtrace is expected evidence from the intentional panic-test path; `catch_unwind` catches it and the test passes.
- `cargo remote -c -- build -p sentinel-daemon --release --features fuse`
  - PASS: `Finished release profile [optimized] target(s) in 57.79s`
  - artifact hash: `4400fcb9162fe242f3cbebda550f25175976abc8f444475ab201d0570e43c3d2  target/release/sentinel-daemon`
- `git diff --check`
  - PASS: no whitespace/conflict errors.

## Task 7 - Slice E: bounded Analysis-/Recovery-Pfade

### Pre-task self-check

- Was muss getan werden: Slice E aus Donor `389b5e6` path-limited portieren, unbounded Analyse-Vorstufen begrenzen, Coalescing-/Drop-Counter sichtbar machen und einen deterministischen Flood-Testhook bereitstellen.
- Welche ACs muessen hier passen:
  - AC-1: Platform-Controlplane-Triggerqueue ist bounded, coalesced Duplikate und droppt aelteste Eintraege bei Ueberlauf.
  - AC-2: LLM-Analyzer-Channel ist bounded und zaehlt Dropped-Requests statt unbounded Memory-Wachstum zuzulassen.
  - AC-3: Runtime-Health enthaelt `analysis_queue_depth`, `analysis_queue_dropped_total` und `analysis_queue_coalesced_total`.
  - AC-4: `POST /operator/runtime/analysis-flood-test` erzeugt kontrollierte Queue-Pressure fuer Live-Verifikation.
  - AC-5: Remote-Tests, FUSE-Release-Build, Clippy, Artefakt-Hash, Scope-Guard und `git diff --check` sind gruen.
- Wie wird bewiesen: Code-Diff, `cargo remote -c -- fmt/test/clippy/build`, `sha256sum`, `git diff --check`, Scope-Guard gegen Haiku-/Model-Pinning.
- Erwartete Dateien: `config/daemon.toml`, `services/sentinel-daemon/src/config.rs`, `operator_api.rs`, `orchestrator.rs`, `platform_controlplane/*`, `runtime_control.rs`, `runtime_health.rs`, `CHANGELOG.md`.
- Risiken:
  - Nur den #279-Queue-Scope portieren, keine Gateway-/Model-Policy.
  - Analyzer- und Controlplane-Stats muessen zusammengefuehrt werden, damit Runtime-Health nicht nur eine Queue sieht.
  - Flood-Test muss ueber Operator-API/Runtime-Control laufen und darf kein ungeschuetzter Produktionspfad werden.

### Outcome

- Path-limited donor Slice E was applied from stack donor `389b5e6`.
- Added `[daemon.platform_controlplane]` settings:
  - `llm_trigger_queue_capacity = 16`
  - `llm_analysis_channel_capacity = 16`
- `PlatformControlplane` now uses a bounded `VecDeque` trigger queue.
- Duplicate queued triggers are coalesced and counted via `analysis_queue_coalesced_total`.
- Overflow drops the oldest trigger and increments `analysis_queue_dropped_total`.
- `PlatformLlmAnalyzerHandle` now uses a bounded channel sized from config and tracks depth/drop counters.
- Runtime-Health now publishes merged controlplane/analyzer queue stats.
- Added `POST /operator/runtime/analysis-flood-test`:
  - rejects `count=0`
  - dispatches through `RuntimeControlCommand::AnalysisFloodTest`
  - injects bounded/coalesced queue pressure in the ECS owner thread
  - returns `requested`, `queue_depth`, `dropped_total` and `coalesced_total`
- No Haiku/model-policy changes were introduced; runtime still leaves model selection to the Gateway default.

### Evidence

- `cargo remote -c -- fmt --check`
  - PASS: no remaining rustfmt diff.
- `cargo remote -c -- test -p sentinel-daemon test_parse_config -- --nocapture`
  - PASS: `2 passed; 0 failed; 188 filtered out`
- `cargo remote -c -- test -p sentinel-daemon trigger_queue -- --nocapture`
  - PASS: `2 passed; 0 failed; 188 filtered out`
- `cargo remote -c -- test -p sentinel-daemon enqueue_drops_when_bounded_queue_is_full -- --nocapture`
  - PASS: `1 passed; 0 failed; 189 filtered out`
- `cargo remote -c -- test -p sentinel-daemon runtime_analysis_flood_test -- --nocapture`
  - PASS: `2 passed; 0 failed; 188 filtered out; finished in 0.06s`
- `cargo remote -c -- clippy -p sentinel-daemon --all-targets --features fuse -- -D warnings`
  - PASS: `Finished dev profile [unoptimized + debuginfo] target(s) in 2m 59s`
- `cargo remote -c -- build -p sentinel-daemon --release --features fuse`
  - PASS: `Finished release profile [optimized] target(s) in 56.44s`
  - artifact hash: `39bf79442190541ec03f11a66c4e8be437f6a8bd1f2e2366611a0d9ad0366cb2  target/release/sentinel-daemon`
- `git diff --check`
  - PASS: no whitespace/conflict errors.
- Scope guard:
  - `rg -n "AGENT_MODEL_HAIKU|model: AGENT|model: String::new\\(\\).*Gateway" services/sentinel-daemon/src cmd crates || true`
  - PASS: only `services/sentinel-daemon/src/llm_bridge.rs:612: model: String::new(), // Gateway waehlt default`

## Task 8 - Slice F: Projection/API-Konvergenz

### Pre-task self-check

- Was muss getan werden: Slice F aus Donor `14560c4` plus relevante Stabilisierung aus `b1e376b` path-limited portieren, Projection-Drift erkennbar machen, Projection-Rebuilds per Request-Datei ausloesen und Restart-Storms vermeiden.
- Welche ACs muessen hier passen:
  - AC-1: Projection-Worker konsumiert eine Rebuild-Request-Datei und entfernt sie nach erfolgreichem Full-Rebuild.
  - AC-2: Projection-Read-Model kann read-only geoeffnet werden, ohne Migrations-/Cleanup-Writes auf Runtime-Health-Reads auszufuehren.
  - AC-3: Handlerfehler rollen Projection-Batches zurueck statt teilweise fortzuschreiben.
  - AC-4: Runtime-Health erkennt Projection-Drift gegen Runtime-/Security-/Cgroup-Truth.
  - AC-5: Runtime-Reconcile fordert bei Drift einen Rebuild an und startet `sentinel-projection` nicht neu, solange der Service aktiv ist und in-place rebuilden kann.
  - AC-6: Remote-Tests, Clippy, Release-Builds, Artefakt-Hashes und Scope-Guard sind gruen.
- Wie wird bewiesen: Code-Diff, `cargo remote -c -- fmt/test/clippy/build`, `sha256sum`, `git diff --check`, Scope-Guard gegen Haiku-/Model-Pinning.
- Erwartete Dateien: `crates/sentinel-projection/*`, `services/sentinel-projection/src/main.rs`, `services/sentinel-daemon/src/runtime_health.rs`, `runtime_control.rs`, `orchestrator.rs`, `service_health.rs`, `operator_api.rs`, `CHANGELOG.md`.
- Risiken:
  - Projection-Recovery darf keine Restart-Schleife erzeugen.
  - Runtime-Health darf keine Projection-Migrationen oder Startup-Cleanup nebenbei ausfuehren.
  - Slice F darf keine FUSE/Landlock- oder Gateway-/Model-Policy-Arbeit einschleppen.

### Outcome

- Path-limited Slice F was applied from stack donor `14560c4` plus relevant stabilization from `b1e376b`.
- `ProjectionConfig` now carries `rebuild_request_path` and a `1s` poll interval.
- `ProjectionWorker` now polls `.projection-rebuild-request`, runs a full rebuild in-place, and removes the request file after success.
- Projection batch handler failures now rollback the transaction and return an error instead of silently skipping failed events.
- `ReadModelStore::open_readonly()` opens Projection DB read-only with a busy timeout and does not run migrations or startup cleanup.
- Runtime-Health uses read-only Projection access and reports:
  - `projection_drift_detected`
  - `projection_drift_agents`
- Runtime-Reconcile writes rebuild request payloads with a reason and reports:
  - `projection_drift_before`
  - `projection_drift_after`
  - `projection_restart_attempted`
  - `projection_restart_succeeded`
- Runtime-Reconcile deliberately skips `systemctl restart sentinel-projection` when Projection is already active and a request-file rebuild can run in-place.
- `service_health::restart_service()` now calls `systemctl reset-failed` before restart when a restart is actually required.
- No Haiku/model-policy changes were introduced; runtime still leaves model selection to the Gateway default.

### Evidence

- `cargo remote -c -- fmt --check`
  - PASS: no remaining rustfmt diff.
- `cargo remote -c -- test -p sentinel-projection rebuild_request -- --nocapture`
  - PASS: `1 passed; 0 failed`
- `cargo remote -c -- test -q -p sentinel-projection -- --nocapture`
  - PASS: `7 unit + 6 acceptance tests passed`
- `cargo remote -c -- test -q -p sentinel-daemon runtime_control -- --nocapture`
  - PASS: `2 passed`
- `cargo remote -c -- test -q -p sentinel-daemon runtime_health -- --nocapture`
  - PASS: `4 passed`
- `cargo remote -c -- test -q -p sentinel-daemon test_runtime_reconcile_skips_projection_restart_when_rebuild_can_run_in_place -- --nocapture`
  - PASS: `1 passed`
- `cargo remote -c -- clippy -q -p sentinel-projection --all-targets -- -D warnings`
  - PASS: exit `0`
- `cargo remote -c -- clippy -q -p sentinel-projection-service --all-targets -- -D warnings`
  - PASS: exit `0`
- `cargo remote -c -- clippy -q -p sentinel-daemon --all-targets --features fuse -- -D warnings`
  - PASS: exit `0`
- `cargo remote -c -- build -q -p sentinel-daemon --release --features fuse`
  - PASS: exit `0`
  - artifact hash: `3a05fc872c061ae973e3d820e0a1aaa2d0a909b201d61e9b0c8c84b080c08425  target/release/sentinel-daemon`
- `cargo remote -c -- build -q -p sentinel-projection-service --release`
  - PASS: exit `0`
  - artifact hash: `d0da3c42b11397e1c954777397c4b3622cfeeb4365fff2adebbded9adacb13b4  target/release/sentinel-projection`
