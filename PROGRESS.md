# PROGRESS

## Status

- Plan source: `/work/company/codex-plan279.md`
- Overall status: `TASK_1_DONE_TASK_2_PENDING`
- Current task: `Task 2 - Phase 1: Donor-Audit und Clean Port`
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
- Der GitHub-Issue-Body ist stale: er enthaelt noch `Blocked by #278`, alte Runtime-Zahlen und muss vor Codearbeit truth-repaired werden.
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

## Blocked items

- Kein harter technischer Blocker beim Setup.
- `mainrag` ist lokal nicht verfuegbar; fuer `#279` nicht blockierend.
- Kein Phase-0-Blocker mehr.
- Der Main-Reset bestaetigt, dass der volle #279-Recovery-Scope notwendig bleibt.

## Commit references

- `TBD` Task [1] Phase 0 - Issue-Repair und Baseline-Reset
- `TBD` Task [2] Phase 1 - Donor-Audit und Clean Port
- `TBD` Task [3] Slice A - Runtime-Health-Read-Model
- `TBD` Task [4] Slice B - Runtime-Control / Reconcile
- `TBD` Task [5] Slice C - Fast Stall Recovery
- `TBD` Task [6] Slice D - Worker-Supervision
- `TBD` Task [7] Slice E - bounded Analysis-/Recovery-Pfade
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
| 2 | Phase 1 - Donor-Audit und Clean Port | PENDING | alte #279-Donor-Commits pruefen, nur scope-konforme Teile uebernehmen, Haiku ausschliessen | inspect, command |
| 3 | Slice A - Runtime-Health-Read-Model | PENDING | `/operator/runtime-health`, Snapshot-Modell, Auth/Loopback, Tests | inspect, command, system |
| 4 | Slice B - Runtime-Control / Reconcile | PENDING | `/operator/runtime/reconcile`, stale/orphan/zombie cleanup, bounded retries | inspect, command, system |
| 5 | Slice C - Fast Stall Recovery | PENDING | deterministischer Kandidat, SIGSTOP/SIGCONT, Respawn/Reconcile | command, system |
| 6 | Slice D - Worker-Supervision | PENDING | catch_unwind + in-process worker respawn, panic-test Hook | inspect, command, system |
| 7 | Slice E - bounded Analysis-/Recovery-Pfade | PENDING | bounded trigger queues, coalescing/drop counters, flood-test | inspect, command, system |
| 8 | Slice F - Projection/API-Konvergenz | PENDING | read-only Projection-Reads, Rebuild-Request, drift-heal | inspect, command, system |
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
