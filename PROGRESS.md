# PROGRESS

## Status

- Plan source: `/work/company/codex-plan264.md`
- Overall status: `TASK_1_DONE_TASK_2_DONE_TASK_3_PENDING`
- Current task: `Task 3 - Write-Anomaly Enforcement, SIGSTOP und Platform-Intervention-Audit (Phase B)`
- Current branch: `feat/issue-264-security-hardening-closure`
- Hook status: `PreToolUse TaskUpdate + PostToolUse start-enforcer projektlokal registriert`
- Last refresh: `2026-04-11 Europe/Vienna`

## Current findings

- Ausgangsstand vor dem Branch-Schnitt: `main = origin/main = 0395905`.
- Arbeitsbranch `feat/issue-264-security-hardening-closure` wurde frisch von diesem Stand angelegt.
- Im Worktree existiert bereits eine fremde tracked Aenderung an `.gitignore`; sie bleibt unangetastet.
- Projektregeln, globale Regeln, Workspace-Handover, repo-lokales `.claude/AGENTS.md` und der komplette `#264`-Plan wurden in dieser Session frisch gelesen.
- Die Start-Hooks sind lokal in `.claude/settings.json` registriert:
  - `~/bin/pretooluse-task-checklist-gate.sh`
  - `~/bin/pretooluse-start-progress-gate.sh`
  - `~/bin/posttooluse-start-enforcer.sh`
- `mainrag search "issue 264 security hardening" --source claude-conversations --limit 5` ist aktuell nicht nutzbar, weil `http://localhost:3001/api/v1/sources` `Connection refused` liefert.
- `gh issue view 264` bestaetigt die Truth-Repair-Ausgangslage:
  - Ausgangsstand war `CLOSED` mit `status:verified`
  - nach Truth-Repair ist `#264` jetzt `OPEN` mit `status:ready`
  - Body ist auf den neuen Stand gezogen: AC-3 bleibt in scope, FUSE-Gate ist explizit, Kette ist korrigiert
- Harte Ausfuehrungsregel aus dem Plan:
  - Ohne klare AC-3-Scope-Entscheidung startet kein Feature-Task jenseits von Task 1.
- Die Scope-Entscheidung fuer diese Ausfuehrung ist jetzt fest:
  - **AC-3 bleibt in `#264` in scope**
  - Wenn FUSE oder Artifact-Restore live nicht tragfaehig sind, ist das ein dokumentierter Blocker fuer `#264`, kein Re-Scope im Vorbeigehen
- Frische Start-Baseline fuer `#264` ist belegt:
  - `services/sentinel-daemon/Cargo.toml` enthaelt `default = [..., "fuse"]` und eigenes `fuse` Feature
  - Live auf `10.0.0.240` ist aktuell **kein** `fs_mount` in `daemon.toml` gesetzt
  - Live auf `10.0.0.240` existiert der Trigger `protect_recent_snapshots`
  - `sentinel-daemon`, `sentinel-gateway` und `sentinel-projection` sind `active`
  - `landlock.rs` nutzt aktuell `all_access` fuer `write_paths`
  - `cockpit.ts` mappt sowohl `platform_analysis` als auch `platform_intervention`
- Task-2-Readiness und Live-Evidence stehen jetzt:
  - `operator_api.rs` exponiert die geplanten loopback-only `/operator/security/*` Hooks fuer Trash, Runtime-State, Write-Anomaly und Landlock
  - `orchestrator.rs` tracked dafuer live `bwrap_pid`, Agent-Home und optionales `fs_mount` in einem shared Runtime-State
  - `ArtifactPlane::open()` initialisiert jetzt auch `fs_trash_queue`; ohne diesen Fix waeren die neuen Trash-Inspect-/GC-Pfade auf leeren Tabellen gelandet
  - `breakout-helper` deckt jetzt die Landlock-Szenarien `exec-from-home`, `exec-bin-sh` und `exec-python3` ab; vorher war nur `exec-from-tmp` implementiert
  - Der Live-Befund auf `10.0.0.240` war erst rot im Detail:
    - `landlock-test` lieferte korrekt `exit_code=1` plus `Permission denied`, aber der Response markierte faelschlich `blocked:false`
  - Der Fix ist jetzt deployed und live bestaetigt:
    - dieselben Exec-Szenarien liefern `blocked:true`
  - Der tatsaechliche API-Contract der Task-2-Hooks ist dokumentiert:
    - `fs-trash-fixture` braucht `agent_name`, `relative_path`, `content`
    - `write-anomaly-test` braucht `agent_name`, `mode`, `bytes_per_sec`

## Blocked items

- Kein harter technischer Blocker beim Setup.
- Bedingter Blocker:
  - Wenn Task 1 die AC-3-Scope-Entscheidung nicht sauber im Issue-/Execution-Stand verankert, bleiben Task 8 bis Task 10 gesperrt.
- Nebenfund:
  - `mainrag` lokal nicht verfuegbar; fuer `#264` aktuell kein technischer Blocker, aber dokumentierter Kontextverlust-Risikoindikator.

## Commit references

- `f089875` Task [1]
- `TBD` Task [2]
- `TBD` Task [3]
- `TBD` Task [4]
- `TBD` Task [5]
- `TBD` Task [6]
- `TBD` Task [7]
- `TBD` Task [8]
- `TBD` Task [9]
- `TBD` Task [10]
- `TBD` Task [11]

## Task table

| # | Task | Status | Scope | Evidence |
|---|------|--------|-------|----------|
| 1 | Issue-Truth-Repair, Branch-/Execution-Setup und AC-3-Scope-Gate | DONE | GitHub-Truth-Repair, Branch-/SSOT-Setup, AC-3-Entscheidung, frische Baseline fuer `#264` | command, system, inspect |
| 2 | Operator-/Admin-Security-Read- und Testpfade (Phase E Foundations) | DONE | `/operator/security/*` Read-/Test-Hooks, Auth/Loopback, deterministische Evidence-Pfade | inspect, command, system |
| 3 | Write-Anomaly Enforcement, SIGSTOP und Platform-Intervention-Audit (Phase B) | PENDING | Rolling Baseline, deterministischer Suspend, `platform_intervention` Audit-Trail | inspect, command, system |
| 4 | Landlock-Minimal-Exec und `sandbox_exec_blocked` Events (Phase C) | PENDING | `write_paths` ohne Execute, minimale Exec-Allowlist, Security-Eventpfad | inspect, command, system |
| 5 | Immutable Snapshot Hardening und Retention-Sicherheit (Phase D) | PENDING | SQLite-Trigger, Retention-/Prune-Vertraeglichkeit, Delete-Pfade | inspect, command, system |
| 6 | Dashboard/Cockpit/Projection/UI-Surfaces und lokale UI-Tests (AC-5) | PENDING | Cockpit-/Dashboard-Surfaces, lokale Bun-/Projection-Tests, deploybare UI-Pfade | inspect, command, browser |
| 7 | Phase-1 Deploy, VM-Verifikation und Benchmarks fuer AC-4 bis AC-8 | PENDING | Release-Build, Deploy, Restarts, AC-4..AC-8, Phase-1-Benchmarks, no panic/no drift | command, system, browser |
| 8 | FUSE-Runtime-Gate und CAS Trash-Queue Implementation fuer AC-1/AC-2 | PENDING | `fuse` Feature-Gate, `fs_mount`, Trash-Queue Runtime-/Inspect-Pfade | inspect, command, system |
| 9 | Artifact-Restore-Integration fuer AC-3 oder formaler Blocker-Pfad nach Scope-Entscheid | PENDING | Restore-Pfad fuer Artifact-Plane oder offizieller Re-Scope-/Blocker-Pfad | inspect, command, system |
| 10 | Phase-2 Deploy, VM-Verifikation und Benchmarks fuer AC-1 bis AC-3 | PENDING | FUSE-/Artifact-Runtime live verifizieren, AC-1..AC-3, Phase-2-Benchmarks | command, system, browser |
| 11 | Plan-Verifikation | PENDING | Plan Zeile fuer Zeile gegen Ergebnis pruefen, Mismatches fixen, Abschlussstatus setzen | inspect, command, system, browser |

## Task details

### Task 1 - Issue-Truth-Repair, Branch-/Execution-Setup und AC-3-Scope-Gate

- Scope:
  - `#264`-Ist-Zustand gegen GitHub, Repo und VM neu erfassen
  - GitHub-Truth-Repair vorbereiten/anwenden
  - Branch-/Execution-SSOT auf `#264` ziehen
  - AC-3-Scope-Entscheidung sauber im Ausfuehrungsstand verankern
- Checklist:
  - `gh issue view 264` gegen [codex-plan264.md](/work/company/codex-plan264.md) abgleichen
  - Branch-/Worktree-Baseline dokumentieren
  - Truth-Repair-Kommandos fuer Reopen/Labels/Body vorbereiten oder anwenden
  - AC-3-Scope-Pfad festhalten: in-scope oder offizieller Re-Scope-/Blocker-Pfad
  - frische Baseline-Facts fuer `fuse`, Landlock, Snapshot-Trigger und Cockpit-Mergepfad dokumentieren
- Acceptance criteria:
  - AC-1: Branch und `PROGRESS.md` sind sauber auf `#264` umgestellt, ohne fremde Worktree-Aenderungen zu verlieren
  - AC-2: GitHub-Truth-Repair fuer `#264` ist vorbereitet oder live angewendet; veraltete Verify-/Kettenannahmen sind explizit adressiert
  - AC-3: AC-3-Scope-Entscheidung ist fuer diese Ausfuehrung festgehalten; downstream FUSE-/Restore-Tasks laufen nicht auf stillen Annahmen
  - AC-4: frische Start-Baseline fuer `#264` liegt mit Repo-/VM-Evidence vor
- Required evidence:
  - AC-1: `command`, `system`
  - AC-2: `command`, `inspect`
  - AC-3: `command`, `inspect`
  - AC-4: `command`, `inspect`, `system`
- Pre-task self-check:
  - Was muss getan werden: die historische Falsch-Schliessung und der echte Startzustand muessen vor jeder Feature-Arbeit neu eingehaengt werden
  - Welche ACs muessen hier passen: AC-1 bis AC-4 dieses Tasks
  - Wie wird bewiesen: Git/GitHub/VM-Kommandos, gezielte Code-Lesepfade, Issue-State
  - Erwartete Dateien: `PROGRESS.md`, ggf. GitHub-Issue-Body, evtl. keine Produktionscode-Files
  - Risiken: AC-3-Scope kann echten Stop verursachen; `mainrag` ist lokal nicht verfuegbar
- Outcome:
  - Arbeitsbranch `feat/issue-264-security-hardening-closure` ist frisch von `main = origin/main = 0395905` angelegt.
  - `#264` wurde von `CLOSED + status:verified` auf `OPEN + status:ready` truth-repaired.
  - Der Issue-Body ist jetzt auf dem ehrlichen Ausfuehrungsstand:
    - aktuelle Luecken benannt
    - FUSE-Compile-Gate explizit
    - AC-3 bleibt in scope
    - Kette `#263 -> #264 -> #265 -> #266` korrigiert
  - Die Scope-Entscheidung fuer diese Session ist festgehalten:
    - AC-3 bleibt in `#264`
    - FUSE-/Artifact-Restore-Probleme gelten als Blocker, nicht als stiller Re-Scope
  - Die frische Start-Baseline fuer FUSE, Landlock, Snapshot-Trigger und Cockpit-Mergepfad ist mit Repo-/VM-Evidence dokumentiert.
- Evidence:
  - AC-1 PASS:
    - `git fetch origin main && git rev-parse main && git rev-parse origin/main && git rev-list --left-right --count main...origin/main` => `main = origin/main = 0395905`, Divergenz `0 0`
    - `git switch -c feat/issue-264-security-hardening-closure` => neuer Arbeitsbranch aktiv
    - `git status --short --branch` => fremde `.gitignore`-Aenderung blieb erhalten; keine fremde Datei verworfen
  - AC-2 PASS:
    - `gh issue reopen 264`
    - `gh issue edit 264 --remove-label "status:verified" --add-label "status:triage" --add-label "quality:needs-spec"`
    - `gh issue edit 264 --body-file - ...`
    - `gh issue edit 264 --remove-label "quality:needs-spec" --remove-label "status:triage" --add-label "status:ready"`
    - `gh issue view 264 --json state,labels,body ...` => `OPEN`, `status:ready`, Body enthaelt AC-3-in-scope, FUSE-Gate und neue Kette
  - AC-3 PASS:
    - User-Entscheidung in dieser Session: `AC-3 bleibt in #264`
    - Issue-Body enthaelt explizit `AC-3 bleibt in diesem Issue in scope`
    - Blocker-Semantik ist damit klar: FUSE-/Artifact-Restore-Probleme blockieren `#264`, sie schneiden AC-3 nicht still aus
  - AC-4 PASS:
    - `rg -n '^default = .*fuse|^fuse =' services/sentinel-daemon/Cargo.toml` => `default = [..., "fuse"]`, eigenes `fuse` Feature vorhanden
    - `ssh ubuntu@10.0.0.240 "grep -n '^fs_mount' /opt/sentinel/config/daemon.toml || true"` => aktuell kein `fs_mount` gesetzt
    - `ssh ubuntu@10.0.0.240 "sqlite3 /opt/sentinel/data/events.db \"SELECT name FROM sqlite_master WHERE type='trigger' AND name='protect_recent_snapshots';\""` => `protect_recent_snapshots`
    - `ssh ubuntu@10.0.0.240 "systemctl is-active sentinel-daemon sentinel-gateway sentinel-projection"` => alle `active`
    - `rg -n 'all_access|write_paths|exec_paths|/tmp|/home/\\{name\\}' crates/sentinel-sandbox/src/landlock.rs` => `write_paths` nutzen `all_access`
    - `rg -n 'platform_analysis|platform_intervention' dashboard/src/routes/cockpit.ts` => beide Eventtypen werden im Cockpit gemappt

### Task 2 - Operator-/Admin-Security-Read- und Testpfade (Phase E Foundations)

- Scope:
  - loopback-only `/operator/security/*` Read- und Test-Hooks fuer deterministische AC-Evidence
- Checklist:
  - `GET /operator/security/fs-trash`
  - `POST /operator/security/fs-trash-fixture`
  - `POST /operator/security/fs-trash-age`
  - `POST /operator/security/fs-trash-gc`
  - `POST /operator/security/fs-ransomware-test`
  - `GET /operator/security/agent-runtime-state`
  - `POST /operator/security/write-anomaly-test`
  - `POST /operator/security/landlock-test`
  - Auth/Loopback-Regeln absichern
- Acceptance criteria:
  - AC-1: alle noetigen Security-Read-/Testpfade existieren und sind nur lokal/admin nutzbar
  - AC-2: Operator-Secret wird korrekt erzwungen, falls gesetzt
  - AC-3: lokale Tests und Remote-Rust-Checks fuer die neuen Pfade sind gruen
- Required evidence:
  - AC-1: `inspect`, `command`
  - AC-2: `inspect`, `system`
  - AC-3: `command`
- Outcome:
  - `services/sentinel-daemon/src/operator_api.rs` exponiert jetzt die geplanten Security-Hooks:
    - `GET /operator/security/fs-trash`
    - `POST /operator/security/fs-trash-fixture`
    - `POST /operator/security/fs-trash-age`
    - `POST /operator/security/fs-trash-gc`
    - `POST /operator/security/fs-ransomware-test`
    - `GET /operator/security/agent-runtime-state`
    - `POST /operator/security/write-anomaly-test`
    - `POST /operator/security/landlock-test`
  - Query-Parsing, loopback-read paths, Auth-Check fuer gesetztes Operator-Secret und body-size routing sind zentral im Operator-API-Pfad verankert.
  - `services/sentinel-daemon/src/orchestrator.rs` fuehrt jetzt einen shared `security_runtime_state` mit `agent_id`, `aggregate_id`, `agent_name`, `bwrap_pid`, Home-Pfad und optionalem `fs_mount`; der State wird beim Spawn, Fallback, Restart und Shutdown mitgefuehrt.
  - `crates/sentinel-fs/src/artifact.rs` initialisiert `fs_trash_queue` beim Open und expose't Test-/Inspect-Helfer fuer Trash-Timestamps.
  - `crates/sentinel-sandbox/src/bin/breakout_helper.rs` deckt jetzt die deterministischen Exec-Szenarien `exec-from-home`, `exec-bin-sh` und `exec-python3` ab.
  - Ein realer Runtime-Bug im neuen Landlock-Testpfad wurde behoben:
    - Landlock blockierte Exec bereits korrekt im Wrapper
    - der API-Response markierte das aber nur bei `exit_code == 0` als `blocked`
    - jetzt wird auch der reale Wrapper-Fehlerpfad `exec failed: Permission denied` bzw. `Operation not permitted` als `blocked:true` gemappt
- Evidence:
  - AC-1 PASS:
    - `rg -n "fs-trash|write-anomaly-test|landlock-test|agent-runtime-state" services/sentinel-daemon/src/operator_api.rs` => alle geplanten `/operator/security/*` Endpunkte sind im Handler verdrahtet
    - `cargo remote -c -- build -p sentinel-daemon --release` => `Finished release profile [optimized] target(s)` fuer den Deploy-Stand
    - `scp target/release/sentinel-daemon ubuntu@10.0.0.240:/tmp/sentinel-daemon` und `ssh ubuntu@10.0.0.240 "sudo systemctl stop sentinel-daemon && sudo install -m 0755 /tmp/sentinel-daemon /opt/sentinel/bin/sentinel-daemon && sudo systemctl start sentinel-daemon && sudo systemctl is-active sentinel-daemon"` => Redeploy erfolgreich, Dienst `active`
    - `curl --max-time 2 http://10.0.0.240:8084/operator/security/agent-runtime-state?agent_id=49` => Timeout von ausserhalb der VM, also kein versehentlich exponierter Remote-Pfad
    - `ssh ubuntu@10.0.0.240 "curl -s http://127.0.0.1:8084/operator/security/agent-runtime-state?agent_id=49"` => `found:true`, `agent_name:"Carla Friedmann"`, `bwrap_pid:...`, `home_host_path:"/ram/agents/Carla Friedmann"`
    - `ssh ubuntu@10.0.0.240 'python3 - <<'"'"'PY'"'"' ... fs-trash-fixture ... PY'` => `accepted:true`, `chunk_hashes:["e926e21be7c88cc5152a7d37fff800fb"]`, `trashed_chunks:1`
    - `ssh ubuntu@10.0.0.240 'python3 - <<'"'"'PY'"'"' ... write-anomaly-test ... PY'` => `accepted:true`, `mode:"burst"`, `bwrap_pid:1161446`, `host_path:"/ram/agents/Carla Friedmann/.issue264-write-anomaly.bin"`
    - `ssh ubuntu@10.0.0.240 "curl -s -X POST http://127.0.0.1:8084/operator/security/landlock-test -H 'Content-Type: application/json' -d '{\"agent_name\":\"Carla Friedmann\",\"scenario\":\"exec-python3\"}'"` => `accepted:true`, `blocked:true`, Wrapper meldet `Permission denied`
    - `ssh ubuntu@10.0.0.240 "curl -s -X POST http://127.0.0.1:8084/operator/security/landlock-test -H 'Content-Type: application/json' -d '{\"agent_name\":\"Carla Friedmann\",\"scenario\":\"exec-bin-sh\"}'"` => ebenfalls `blocked:true`
  - AC-2 PASS:
    - `cargo remote -c -- test -p sentinel-daemon operator_api::tests -- --nocapture` => Test `security_get_requires_auth_when_configured` gruen
    - `rg -n "is_security_path|is_authorized|shared_secret" services/sentinel-daemon/src/operator_api.rs` => GET-Security-Pfade und POSTs haengen am zentralen Secret-Check
    - Live auf `10.0.0.240` liefert nur Loopback Antworten; der Host `10.0.0.240:8084` bleibt von aussen nicht erreichbar
  - AC-3 PASS:
    - `cargo remote -c -- test -p sentinel-daemon operator_api::tests -- --nocapture` => `21 passed`
    - `cargo remote -c -- test -p sentinel-daemon orchestrator::tests -- --nocapture` => Exit `0`
    - `cargo remote -c -- test -p sentinel-fs artifact::tests -- --nocapture` => `5 passed`
    - `cargo remote -c -- clippy -p sentinel-daemon -p sentinel-fs --all-targets -- -D warnings` => Exit `0`
    - `cargo remote -c -- clippy -p sentinel-sandbox --all-targets -- -D warnings` => Exit `0`

### Task 3 - Write-Anomaly Enforcement, SIGSTOP und Platform-Intervention-Audit (Phase B)

- Scope:
  - Rolling-Baseline, Write-Anomaly-Trigger, echter `SIGSTOP`, Audit-Trail ueber `platform_intervention`
- Checklist:
  - Baseline-Berechnung erweitern
  - Triggerlogik `>10x` oder absoluter Schwellwert
  - deterministischen SIGSTOP-Sideeffect bauen
  - `platform_intervention` fuer `write_anomaly` sauber persistieren
  - Tests fuer Baseline, Trigger, SIGSTOP, Audit schreiben
- Acceptance criteria:
  - AC-1: Write-Anomaly feuert nicht mehr nur `alert`, sondern echten Suspend-Sideeffect
  - AC-2: Audit-Trail laeuft ueber eindeutigen Eventtyp und korreliert mit Ziel-Agent/Action
  - AC-3: lokale Tests und Remote-Rust-Checks sind gruen
- Required evidence:
  - AC-1: `inspect`, `command`, `system`
  - AC-2: `inspect`, `command`
  - AC-3: `command`

### Task 4 - Landlock-Minimal-Exec und `sandbox_exec_blocked` Events (Phase C)

- Scope:
  - `write_paths` ohne Execute, minimale Exec-Allowlist, strukturierter Security-Eventpfad
- Checklist:
  - `landlock.rs` fuer non-exec write paths umbauen
  - minimale Exec-Allowlist definieren
  - `/tmp`, `/home/{agent}`, `/bin/sh`, `/usr/bin/python3` explizit abdecken
  - neues `sandbox_exec_blocked` Eventmodell + Persistenz bauen
  - Wrapper-/Breakout-/Daemon-Pfade zusammenfuehren
- Acceptance criteria:
  - AC-1: Execute ist weder ueber zu breite `exec_paths` noch ueber `write_paths/all_access` moeglich
  - AC-2: blockierte Exec-Versuche landen strukturiert im Event Store
  - AC-3: lokale Tests und Remote-Rust-Checks sind gruen
- Required evidence:
  - AC-1: `inspect`, `command`, `system`
  - AC-2: `inspect`, `command`
  - AC-3: `command`

### Task 5 - Immutable Snapshot Hardening und Retention-Sicherheit (Phase D)

- Scope:
  - Snapshot-DELETE-Trigger, Retention-/Promotion-/Prune-Kompatibilitaet
- Checklist:
  - Trigger-/Delete-Pfade lesen und haerten
  - Tests fuer `<7 Tage` blockiert, `>7 Tage` erlaubt, Promotion ohne Delete
  - Retention-/Prune-Nebenpfade pruefen
- Acceptance criteria:
  - AC-1: Snapshots juenger als 7 Tage bleiben undeletable
  - AC-2: erlaubte Delete-/Retention-Pfade bleiben funktionsfaehig
  - AC-3: lokale Tests und Remote-Rust-Checks sind gruen
- Required evidence:
  - AC-1: `inspect`, `command`, `system`
  - AC-2: `inspect`, `command`
  - AC-3: `command`

### Task 6 - Dashboard/Cockpit/Projection/UI-Surfaces und lokale UI-Tests (AC-5)

- Scope:
  - Cockpit-/Dashboard-Surfaces fuer Write-Anomaly-/Intervention-Incidents, lokale Bun-/Projection-Tests, stabile Playwright-Ziele
- Checklist:
  - Cockpit-Route und Summary-/Severity-Mapping pruefen/erweitern
  - stabile DOM-Selektoren fuer Incident-Readout sichern
  - `bun test` + `bun run typecheck`
  - falls noetig Projection-Read-Model-Tests
- Acceptance criteria:
  - AC-1: UI kann Write-Anomaly-/Suspend-Fall ueber `platform_analysis` oder `platform_intervention` sichtbar machen
  - AC-2: lokale Dashboard-/Projection-Tests sind gruen
  - AC-3: Deploy-Pfad fuer Dashboard/Projection ist bei Bedarf klar und getestet
- Required evidence:
  - AC-1: `inspect`, `browser`
  - AC-2: `command`
  - AC-3: `command`, `system`

### Task 7 - Phase-1 Deploy, VM-Verifikation und Benchmarks fuer AC-4 bis AC-8

- Scope:
  - Release-Build, Deploy, Restarts, AC-4..AC-8 auf `10.0.0.240`, Benchmarks, no panic/no drift
- Checklist:
  - Remote Builds fuer geaenderte Artefakte
  - Deploy `sentinel-daemon`, `landlock-wrapper`, ggf. Dashboard/Projection
  - AC-4..AC-8 live verifizieren
  - Playwright-Screenshot fuer AC-5
  - Benchmarks + Sidecar-Metriken fahren
  - `panic`-/`drift`-Check fahren
- Acceptance criteria:
  - AC-1: AC-4 bis AC-8 sind live mit frischer VM-Evidence belegt
  - AC-2: Phase-1-Benchmarks liegen gegen Zielwert vor
  - AC-3: keine `panic`- und keine `drift`-Regression im Verifikationsfenster
- Required evidence:
  - AC-1: `command`, `system`, `browser`
  - AC-2: `command`, `system`
  - AC-3: `command`, `system`

### Task 8 - FUSE-Runtime-Gate und CAS Trash-Queue Implementation fuer AC-1/AC-2

- Scope:
  - `fuse` Feature-Gate, `fs_mount`, Runtime-Pfade fuer CAS Trash-Queue, deterministische Inspect-Pfade
- Checklist:
  - `fuse` Build-/Config-Gate final absichern
  - Trash-Queue Runtime-/Inspect-Pfade an aktive FUSE-/Artifact-Plane anbinden
  - Tests fuer Grace-Period und Freigabe nachziehen
- Acceptance criteria:
  - AC-1: FUSE-Runtime-Gate ist echt, nicht nur Config-Schein
  - AC-2: Trash-Queue laeuft im aktiven Runtime-Pfad und nicht nur im isolierten Testpfad
  - AC-3: lokale Tests und Remote-Rust-Checks sind gruen
- Required evidence:
  - AC-1: `inspect`, `command`, `system`
  - AC-2: `inspect`, `command`
  - AC-3: `command`

### Task 9 - Artifact-Restore-Integration fuer AC-3 oder formaler Blocker-Pfad nach Scope-Entscheid

- Scope:
  - Restore-Pfad fuer Artifact-Plane integrieren oder den formal erlaubten Re-Scope-/Blocker-Pfad sauber umsetzen
- Checklist:
  - Snapshot-/Restore-Pfade fuer Artifact-Plane lesen/anpassen
  - `restore_from_trash()` korrekt in den Runtime-Restore einhaengen
  - Falls Scope anders entschieden: offiziellen Re-Scope-/Blocker-Pfad auf Issue-Ebene ziehen
- Acceptance criteria:
  - AC-1: AC-3 ist technisch geliefert oder formal sauber aus `#264` herausgeschnitten
  - AC-2: kein stiller Architekturbruch zwischen Snapshot, ECS und Artifact-Plane
  - AC-3: lokale Tests und Remote-Rust-Checks sind gruen
- Required evidence:
  - AC-1: `inspect`, `command`, `system`
  - AC-2: `inspect`, `command`
  - AC-3: `command`

### Task 10 - Phase-2 Deploy, VM-Verifikation und Benchmarks fuer AC-1 bis AC-3

- Scope:
  - FUSE-/Artifact-Runtime deployen und AC-1..AC-3 live verifizieren
- Checklist:
  - `sentinel-daemon --features fuse` remote bauen und deployen
  - `fs_mount` live setzen und verifizieren
  - AC-1..AC-3 auf der VM fahren
  - Phase-2-Benchmarks und Sidecar-Metriken dokumentieren
- Acceptance criteria:
  - AC-1: AC-1 und AC-2 sind live ueber echten FUSE-/Artifact-Runtime-Pfad belegt
  - AC-2: AC-3 ist live bytegenau verifiziert oder der formale Re-Scope-/Blocker-Pfad ist durch Task 9 hergestellt
  - AC-3: Benchmarks, `panic`- und `drift`-Checks sind gruen
- Required evidence:
  - AC-1: `command`, `system`
  - AC-2: `command`, `system`
  - AC-3: `command`, `system`, `browser`

### Task 11 - Plan-Verifikation

- Scope:
  - [codex-plan264.md](/work/company/codex-plan264.md) vollstaendig gegen das Ergebnis pruefen und letzte Mismatches sofort schliessen
- Checklist:
  - Plan komplett rereaden
  - Ergebnis Zeile fuer Zeile gegen Plan vergleichen
  - finale Live-/E2E-Verifikation fahren
  - Restabweichungen sofort fixen oder als echten Blocker dokumentieren
  - `PROGRESS.md` final auf `COMPLETE` oder `BLOCKED` setzen
- Acceptance criteria:
  - AC-1: kein ungepruefter Plan-/Implementierungs-Mismatch bleibt offen
  - AC-2: Abschlussstatus ist ehrlich (`COMPLETE` oder `BLOCKED`)
  - AC-3: Issue-Close-Vorbereitung folgt erst nach finaler Evidence
- Required evidence:
  - AC-1: `inspect`, `command`, `system`, `browser`
  - AC-2: `inspect`, `command`
  - AC-3: `command`, `system`
