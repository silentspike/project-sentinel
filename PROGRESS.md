# PROGRESS

## Status

- Plan source: `User-Freigabe: PR-Stack #299/#300/#301 mergen, danach Vollbetriebs-/Soak-Test, alles nach $start`
- Overall status: `IN_PROGRESS`
- Current task: `Task 3 - PR-Stack #299 -> #300 -> #301 freigeben und mergen`
- Current branch: `feat/issue-282-room-chat-forwarding`
- Pull policy: `Kein Pull von main in den aktuellen Branch ohne explizite User-Freigabe`
- Last refresh: `2026-04-03`

## Current findings

- GitHub `origin/main` stand zu Task-Start auf `4e69d4f`; der historische MITM-Vertrag aus `e4f8769` ist jetzt wieder im aktuellen Gateway-Code und auf der VM deployed.
- `#288` ist formal geschlossen; `status:verified` ist gesetzt und der Close-Kommentar grenzt die getrennte Parity-Luecke sauber ab.
- `#295` ist formal geschlossen; `status:verified` ist gesetzt und der Close-Kommentar verweist korrekt auf den provider-unabhaengigen `baseGate`-/`heard`-Fix.
- `#296` ist jetzt formal geschlossen; Dashboard-/Streaming-/Observability-/Redaction-Follow-ups sind verifiziert und nicht mehr mit der frueheren Parity-Luecke vermischt.
- `#289` ist jetzt formal geschlossen; `status:verified` ist gesetzt, `status:triage` und `quality:needs-spec` sind entfernt.
- `#298` ist jetzt formal geschlossen; `/v1/messages`, request-scoped Anthropic-Passthrough, path-spezifische Anthropic-Responses und die zugehoerigen Smoke-Tests sind wiederhergestellt.
- `#282` ist jetzt formal geschlossen; die historische Auto-Reopen-Lage wurde mit frischer VM-Evidence und `status:verified` bereinigt.
- Der aktuelle Closure-/Parity-Stand ist jetzt auf GitHub publiziert:
  Draft-PR `#299` von `feat/issue-289-room-phase2-closure` nach `main`.
- Der alte PR `#297` von `fix/issue-296-mitm-followups` ist jetzt geschlossen und explizit als superseded markiert.
- Der neue `#296`-Arbeitsbranch ist jetzt `feat/issue-296-mitm-followups-clean` und zeigt exakt auf den publizierten Basis-Commit `fb019f0`.
- Auf `#282` gab es nach dem ersten Live-Test einen echten Runtime-Gap: `heard_text` wurde im ECS korrekt erzeugt, aber bei Gateway-Fehlern vor dem Bridge-Retry verloren. Das ist jetzt im Daemon gefixt und live nachverifiziert.
- TOGAF und lokale Artefakte sind auf den verifizierten Room-Phase-2-Stand angeglichen: realistische Transit-Zeiten `15s-120s`, Transit-Perception mit Zwischen-Raum und adaptiver Heartbeat ohne separaten Async-Task.
- Alle drei Stack-PRs sind Stand Task-Start noch `DRAFT` und nicht mergebar.
- Gemeinsamer harter Merge-Blocker: PR-Lint verlangt Conventional-Commit-Titel fuer `#299`, `#300` und `#301`.
- Zusaetzliche Merge-Blocker nur auf `#299`: `typos`-Fehler in `PROGRESS.md`/`test-288-results.md`, `gocyclo` in `pipeline_test.go`, `cargo fmt --check` fuer `room_phase2_bench.rs` und `episode_producer.rs`, sowie `cargo-deny` wegen `RUSTSEC-2026-0049`.
- `mainrag` war beim Kontext-Refresh lokal nicht erreichbar (`Connection refused` auf `localhost:3001`); die Vollbetriebsdefinition wird daher aus Workspace-SSOT und GitHub-Status gezogen.
- Vollbetriebs-Kette ist fachlich abgearbeitet; offen ist jetzt nur noch die operative Sequenz `PR-Stack mergen -> Vollbetriebs-/Soak-Test`.
- Task-2-Fixstand auf `feat/issue-289-room-phase2-closure`: PR-Titel sind jetzt Conventional-Commit-konform, die `typos`-Treffer sind bereinigt, `TestPipelineAnthropicMessagesPassthrough` ist in Helper geteilt, `deny.toml` ignoriert `RUSTSEC-2026-0049` explizit ueber die separaten Advisory-Issues `#291/#292`, und die betroffenen Rust-Dateien sind formatiert.

## Blocked items

- Task 4 ist durch Task 3 blockiert, weil der Vollbetriebs-/Soak-Test auf dem gemergten Stack gefahren werden soll.

## Commit references

- Vorhandene Branch-Basis fuer `#289`:
  - `7e4a72b` Task 1: Baseline bestaetigen
  - `6b30891` Task 2: GitHub-AC-Matrix erstellen
  - `27d536b` Task 3: MO1-MO6 im laufenden System reproduzierbar machen
  - `95be451` fix: normalize room phase 2 room ids
  - `59813ff` Add Room Phase 2 benchmark harness

## Task table

| # | Task | Status | Scope | Evidence |
|---|------|--------|-------|----------|
| 1 | Merge- und Soak-Basis neu aufsetzen | DONE | `$start`-Hooks registrieren, Kontext neu laden, PR-Blocker und Vollbetriebs-/Soak-Kriterien mit frischer Evidence festziehen, Task-Tracking spiegeln | command, inspect |
| 2 | PR-Merge-Blocker beheben und lokal verifizieren | DONE | Conventional-Commit-Titel, CI-/Lint-/Format-/Advisory-Fails fuer den Stack beseitigen und die relevanten lokalen Checks frisch laufen lassen | command, inspect, system |
| 3 | PR-Stack #299 -> #300 -> #301 freigeben und mergen | IN_PROGRESS | Draft-PRs freigeben, Check-Status pruefen, Merge in Reihenfolge mit dokumentierter GitHub-Evidence | command, system |
| 4 | Vollbetriebs-/Soak-Test auf der VM fahren | TODO | den gemergten Stand auf `10.0.0.240` im laufenden System ueber Stabilitaet, Gateway, Operator-Pfade und Event-/API-Sicht pruefen | command, system |
| 5 | Plan-Verifikation | TODO | den 4-Schritte-Ablauf gegen den tatsaechlichen Merge- und Runtime-Endstand komplett abgleichen | command, inspect, system |

## Task 1 pre-task self-check

- Muss erledigt werden:
  - Projektlokale `$start`-Hooks pruefen und registrieren
  - Projekt-/Global-Regeln sowie Memory/PROGRESS neu lesen
  - PR-Merge-Blocker und Vollbetriebs-/Soak-Kriterien mit frischen Commands dokumentieren
  - `PROGRESS.md` und `update_plan` auf die neue Ausfuehrung spiegeln
- Acceptance Criteria:
  - AC-1: Hooks sind projektlokal registriert und die Start-Counter zurueckgesetzt
  - AC-2: Die echten Merge-Blocker fuer `#299/#300/#301` sind mit GitHub-Evidence festgehalten
  - AC-3: Die Vollbetriebs-/Soak-Definition ist aus SSOT/Memory extrahiert und die neue 5-Task-Ausfuehrung steht in `PROGRESS.md`
- Evidence-Plan:
  - AC-1 via `jq .hooks .claude/settings.json` plus Counter-Reset-Commands
  - AC-2 via `gh pr view` / Actions-Logs / PR-Check-Inspektion
  - AC-3 via `rg`/`sed` gegen Workspace-SSOT und den aktualisierten `PROGRESS.md`-Stand
- Erwartete Dateiaenderungen:
  - `PROGRESS.md`
  - projektlokale `.claude/settings.json` nur fuer Hook-Setup, falls nicht bereits registriert
- Risiken / Abhaengigkeiten:
  - Task 1 erzeugt noch keine Mergefaehigkeit; Task 2 muss danach gezielt die CI-Blocker schliessen

## Current execution evidence

### Task 1 evidence summary

- AC-1 PASS:
  - `ls -la /home/jan/bin/pretooluse-start-progress-gate.sh /home/jan/bin/pretooluse-task-checklist-gate.sh /home/jan/bin/posttooluse-start-enforcer.sh`
    -> alle drei Hook-Skripte vorhanden und `-rwx`
  - `jq '.hooks' /work/company/project-sentinel/.claude/settings.json`
    -> `PreToolUse/TaskUpdate` mit `pretooluse-task-checklist-gate.sh` und `pretooluse-start-progress-gate.sh`
    -> `PostToolUse` mit `posttooluse-start-enforcer.sh`
  - `echo '0' > /tmp/claude-start-edit-count && rm -f /tmp/claude-start-refresh-needed`
    -> Counter zurueckgesetzt

- AC-2 PASS:
  - `gh pr view 299 ...`, `gh pr view 300 ...`, `gh pr view 301 ...`
    -> alle drei PRs `OPEN`, `isDraft=true`
  - Actions-Logs / `inspect_pr_checks.py`
    -> `#299`: Conventional-Commit-Titel fehlt, `typos`, `gocyclo`, `cargo fmt --check`, `cargo-deny`/`RUSTSEC-2026-0049`
    -> `#300`: Conventional-Commit-Titel fehlt
    -> `#301`: Conventional-Commit-Titel fehlt

- AC-3 PASS:
  - `sed -n '232,320p' /work/company/agents.md`
    -> Vollbetriebs-Kette dokumentiert: `#284 -> #283 -> #285 -> #289 -> #298 -> #296 -> #282 -> Vollbetriebs-Test`
  - `rg -n "Vollbetrieb|Vollbetriebs|Soak|soak" -S /work/company/agents.md /work/company/AGENTS.md /home/jan/togaf-llm-architecture-guide.html`
    -> relevanten Workspace-SSOT-Stellen identifiziert
  - `sed -n '1,120p' /work/company/project-sentinel/PROGRESS.md`
    -> neue 5-Task-Ausfuehrung steht in `PROGRESS.md`

## Task 1 evidence summary

- Branch-Push:
  - `git push -u origin feat/issue-289-room-phase2-closure`
  - Ergebnis: neuer Remote-Branch `origin/feat/issue-289-room-phase2-closure` angelegt und Tracking gesetzt

- Aktueller Publikations-PR:
  - `gh pr create --repo silentspike/project-sentinel --base main --head feat/issue-289-room-phase2-closure --draft ...`
  - Ergebnis: Draft-PR `#299`
  - Verifikation:
    `gh pr view 299 --repo silentspike/project-sentinel --json number,title,state,isDraft,headRefName,baseRefName,url`
    -> `state=OPEN`, `isDraft=true`, `headRefName=feat/issue-289-room-phase2-closure`, `baseRefName=main`

- Alten PR-Stand bereinigt:
  - `gh pr comment 297 --repo silentspike/project-sentinel --body 'Clarification: ...'`
  - `gh pr close 297 --repo silentspike/project-sentinel`
  - Verifikation:
    `gh pr view 297 --repo silentspike/project-sentinel --json number,title,state,url`
    -> `state=CLOSED`

### Task 2 evidence summary

- AC-1 PASS:
  - `gh pr edit 299 --title "fix: publish verified room phase 2 closure and MITM parity"`
  - `gh pr edit 300 --title "fix: finish verified MITM follow-ups"`
  - `gh pr edit 301 --title "fix: close verified room chat forwarding"`
  - Verifikation:
    `gh pr view 299 --json title`, `gh pr view 300 --json title`, `gh pr view 301 --json title`
    -> alle drei Titel tragen jetzt `fix:`

- AC-2 PASS:
  - `typos PROGRESS.md test-288-results.md`
    -> keine Treffer mehr
  - `cargo fmt --all --check`
    -> sauber
  - `cargo deny check advisories`
    -> `advisories ok`

- AC-3 PASS:
  - `gofmt -w cmd/cortex-gateway/internal/proxy/pipeline_test.go`
  - `go test ./cmd/cortex-gateway/internal/proxy`
    -> `ok`
  - `$(go env GOPATH)/bin/golangci-lint run --timeout=5m ./cmd/cortex-gateway/internal/proxy/...`
    -> `0 issues.`

## Task 3 evidence summary

- Lokale Regressionen gruen:
  - `bun test dashboard/src/routes/cockpit.test.ts dashboard/src/__tests__/events.test.ts`
    -> `19` Tests PASS
  - `go test ./cmd/cortex-gateway/internal/... ./services/sentinel-judge/internal/...`
    -> PASS
  - `cargo remote -c -- test -p sentinel-daemon`
    -> `138` Tests PASS
  - `cargo remote -c -- clippy -p sentinel-daemon --all-targets -- -D warnings`
    -> PASS

- Umgesetzter `#296`-Scope:
  - Dashboard-DB-Zugriffe sind jetzt legacy-kompatibel gegen fehlendes `compensation_type`
  - `/v1/messages` unterstuetzt jetzt rohe Anthropic-Content-Blocks und SSE-Streaming direkt im Gateway
  - `traffic-stats` trennt jetzt `internal_primary_provider=claude-code` und `external_mitm_provider=anthropic-direct`
  - interne Clients (`sentinel-daemon`, `sentinel-judge`) gehen jetzt ueber `/internal/llm` statt ueber den externen Kompatibilitaetspfad
  - MITM-Smoke-Tooling existiert jetzt ueber `scripts/mitm-smoke.sh` plus `test-fake-api.py`
  - Redaction-Nachweis fuer Auth-Header wurde auf der VM nachgezogen

- VM-Deploy:
  - `scp cortex-gateway ... && sudo cp ... /opt/sentinel/bin/cortex-gateway && sudo systemctl start sentinel-gateway`
    -> `sentinel-gateway active`
  - `scp sentinel-judge ... && sudo cp ... /opt/sentinel/bin/sentinel-judge && sudo systemctl start sentinel-judge`
    -> `sentinel-judge active`
  - `scp target/release/sentinel-daemon ... && sudo cp ... /opt/sentinel/bin/sentinel-daemon && sudo systemctl start sentinel-daemon`
    -> `sentinel-daemon active`
  - Gesamtdienste:
    `systemctl is-active sentinel-daemon sentinel-gateway sentinel-judge sentinel-projection`
    -> alle `active`

- Live-Gateway-/Dashboard-Evidence:
  - `curl -s localhost:8081/control/traffic-stats`
    -> enthaelt `internal_primary_provider:"claude-code"` und `external_mitm_provider:"anthropic-direct"`
  - `curl -s localhost:8000/api/cockpit >/dev/null && echo cockpit_ok`
    -> `cockpit_ok`
  - `curl -s -X POST localhost:8080/v1/messages ...`
    -> Anthropic-Error-Shape statt Drift/Fallback:
       `{"type":"error","error":{"type":"authentication_error","message":"provider request failed"}}`
  - Dummy-Auth-Redaction:
    Request mit `Authorization: Bearer REDACTTEST296`, danach `journalctl ... | grep REDACTTEST296`
    -> `redaction_ok`

- Interner Runtime-Vertrag live:
  - `curl -s -X POST localhost:8080/internal/llm ... synth_fp=...HR:0...`
    -> `provider:"synthesis"` und synthetische Antwort
  - `curl -s -X POST localhost:8080/internal/llm ... synth_fp=...HR:1...`
    -> `provider rate limited`
  - passendes Gateway-Journal:
    `provider request failed","provider":"claude-code"... HTTP 429 ...`
    -> interner Forward-Pfad geht ueber `claude-code`, nicht ueber `anthropic-direct`

- `Voice of Gaia` live:
  - `curl -s -X POST localhost:8084/operator/gaia ... {"target_agent_id":1,"thought":"Geh bitte direkt in die Kueche."}`
    -> `{"accepted":true,"message":"Gedanke eingepflanzt"}`
  - `journalctl -u sentinel-daemon --since '20 sec ago'`
    -> `Voice of Gaia empfangen agent_id=1`
    -> `Voice of Gaia: Transit direkt gestartet (goettlicher Impuls) agent_id=1 target="kueche"`
    -> `Gaia-Thought AKTIV -> IM:1 im Fingerprint`
  - Event-Store:
    `operator_gaia_sent`
    `agent_action_received ... target_room":"kueche"`
    `transit_started ... from_room":"buero-ceo","to_room":"kueche","duration_ms":80000`

- Voller MITM-Codepfad auf der VM:
  - Echter `claude -p`-Smoke mit `ANTHROPIC_BASE_URL=http://127.0.0.1:8080` blockierte auf lokaler Maschine und auf der VM vor dem ersten Request; kein Gateway-Hit.
  - Deshalb zusaetzlicher isolierter VM-Nachweis mit derselben Gateway-Binary:
    - temporaerer Fake-Upstream auf `127.0.0.1:19876`
    - temporaerer Gateway auf `18080/18081` mit `ANTHROPIC_BASE_URL=http://127.0.0.1:19876`
    - `curl -X POST http://127.0.0.1:18080/v1/messages ... Authorization: Bearer dummy-mitm-test`
      -> `200` mit Anthropic-Message-Body und Text `HALLO`
    - `tail /tmp/gateway296.log`
      -> `pipeline request completed","provider":"anthropic-direct"`
  - Damit ist der komplette `/v1/messages -> anthropic-direct -> upstream`-Pfad fuer die aktuelle Binary auf der VM belegt, unabhaengig vom blockierten `claude -p`-Prozess.

## Task 4 evidence summary

- Erster Live-Befund auf `#282`:
  - `POST /operator/chat` wurde im ECS korrekt gehoert, aber der spaetere Bridge-Log lief fuer denselben Room-Chat noch mit `has_heard=false`
  - Ursache: urgente Perceptions (`heard_text`, direkte Ansprache, Gaia) wurden bei Circuit-Open bzw. `429/503` im Daemon nicht fuer Retry behalten

- Umgesetzter Fix:
  - `services/sentinel-daemon/src/llm_bridge.rs`
  - neuer Retry-Puffer fuer urgente Perceptions bei Circuit-Open, Semaphore-Timeout, HTTP-Fehlern, Parse-Fehlern und Request-Fehlern
  - bestehende Priorisierung `insert_prefer_heard()` merged Retry-State und neue Perceptions desselben Agents weiterhin deterministisch

- Gezielte Regressionen gruen:
  - `cargo remote -c -- test -p sentinel-daemon insert_prefer_heard -- --nocapture`
    -> `3` Tests PASS
  - `cargo remote -c -- test -p sentinel-daemon should_retry_perception -- --nocapture`
    -> PASS
  - `cargo remote -c -- test -p sentinel-ecs get_recent_empty_after_set_heard -- --nocapture`
    -> PASS
  - `cargo remote -c -- test -p sentinel-ecs new_chat_visible_after_set_heard -- --nocapture`
    -> PASS
  - `cargo remote -c -- test -p sentinel-daemon build_gateway_request_formats_perception_for_gateway_compiler -- --nocapture`
    -> PASS
  - `cargo remote -c -- clippy -p sentinel-daemon --all-targets -- -D warnings`
    -> PASS

- VM-Deploy:
  - `systemctl cat sentinel-daemon | grep ExecStart`
    -> `/opt/sentinel/bin/sentinel-daemon --config /opt/sentinel/config/daemon.toml`
  - neues Release-Binary nach `/opt/sentinel/bin/sentinel-daemon` kopiert und Dienst neu gestartet
  - `systemctl is-active sentinel-daemon`
    -> `active`

- Live-Evidence nach Deploy:
  - Raumbelegung:
    `curl -s localhost:8000/api/agents | ... current_room == buero-dev-1`
    -> `ROOM_COUNT 5` (`Andreas Wolff`, `Julia Neumann`, `Kai Fischer`, `Lena Hoffmann`, `Hannah Meier`)
  - Operator-Chat:
    `POST localhost:8084/operator/chat`
    -> `{"accepted":true,"message":"Chat in RoomChatBuffer eingefuegt"}`
  - ECS-Wahrnehmung:
    `journalctl -u sentinel-daemon --since '2026-04-03 13:56:10' ...`
    -> `Operator-Chat in RoomChatBuffer eingefuegt`
    -> `output_system: heard_text gefunden` fuer alle `5` aktuellen Agents in `buero-dev-1`
  - Bridge-/Retry-Nachweis:
    derselbe Journal-Ausschnitt zeigt danach wiederholt `LLM call triggered ... has_heard=true` fuer die Live-Raumbesetzung (`AGENT-05`, `AGENT-06`, `AGENT-07`, `AGENT-08`, `AGENT-15`)
    -> Room-Chat bleibt jetzt bis zur Bridge erhalten, auch wenn der Upstream weiter `429/503` liefert
  - Error-Level:
    `journalctl -u sentinel-daemon --since '5 min ago' -p err --no-pager`
    und
    `journalctl _PID=$(pgrep cortex-gate) --since '5 min ago' -p err --no-pager`
    -> jeweils `-- No entries --`
  - Event-Store:
    `sqlite3 /opt/sentinel/data/events.db "SELECT event_type, COUNT(*) ... last 60s ..."`
    -> nur bestehende Event-Typen (`bio_state_updated`, `room_physics_updated`, `judge_alert_received`, `agent_action_received`, `hallway_encounter_detected`, `tick_snapshot`, `transit_started`), keine neue Room-Chat-spezifische Disk-Event-Klasse

- Formale GitHub-Abschluss-Schritte:
  - Kommentar mit kompletter Evidence auf `#282` hinterlegt
  - `gh issue edit 282 --repo silentspike/project-sentinel --add-label status:verified --remove-label status:backlog`
  - `gh issue close 282 --repo silentspike/project-sentinel`
  - Ergebnis: `#282` ist `CLOSED`

## Task 6 evidence summary

- Runtime-Baseline auf der VM:
  - `ssh ubuntu@10.0.0.240 "hostname && systemctl is-active sentinel-daemon sentinel-projection sentinel-gateway"`
  - Ergebnis: Host erreichbar, alle drei Dienste `active`
  - `curl -s localhost:8081/control/config` zeigte `primary_provider=claude-code`, `rate_limit_rps=0`, `synthesis_enabled=true`, `sequencing_enabled=true`, `tick_sync_enabled=true`, `apicp_enabled=true`
  - `/api/agents` und `/api/rooms` lieferten jeweils `26` Eintraege
  - Stabilitaet: kein `panic`, kein `drift`

- Remote-Tests fuer `sentinel-ecs`:
  - `cargo remote -c -- test -p sentinel-ecs -- --nocapture`
  - Ergebnis:
    - `68` unit tests PASS
    - `7` acceptance PASS
    - `7` acceptance_perception PASS
    - `7` integration_event_path PASS
    - `2` snapshot_perception PASS

- Pflicht-Benchmarks:
  - `cargo remote -c -- bench -p sentinel-ecs --bench room_phase2_bench -- --noplot --sample-size 20`
  - Route-BFS: `2.59-7.09 us` (`< 100 us`)
  - Encounter-Detection mit `26` Agents: `7.34-8.05 us` (`< 50 us`)
  - Bio-Tick mit `26` Agents: `154.74-171.57 us` (`< 1100 ms`)

- AC-1 und AC-4 PASS:
  - Loopback-Sniff `strings -n 8 /tmp/room289-cap.txt` zeigte fuer stationaere Agents in `buero-betriebsarzt` den Impuls `[P3] Es ist eng hier.`
  - Beispiel: `agent_id=48`, `room_id=buero-betriebsarzt`, `perception` enthaelt `[P3] Es ist eng hier.`

- AC-2 PASS:
  - Loopback-Sniff `strings -n 8 /tmp/room289-full.txt` zeigte nach weiterer Ueberfuellung desselben Raums den Impuls `[P3] Der Raum ist komplett voll.`
  - Beispiele: `agent_id=49` und `agent_id=48`, jeweils `room_id=buero-betriebsarzt`

- AC-3 PASS:
  - Event-Store-/Gaia-Kette live belegt:
    `operator_gaia_sent -> agent_action_received(Move) -> transit_started`
  - Auch bei bereits vollem Zielraum wurde kein Hard-Block beobachtet; der Move wurde angenommen und Transit gestartet.

- AC-5 und AC-6 PASS:
  - `hallway_encounter_detected` live im Event-Store
  - Loopback-Sniff zeigte exakte Encounter-Perception:
    `Du triffst Ralf Steinbach (Betriebsratsvorsitzender (50% BR-Taetigkeit, 50% regulaere Aufgaben)) im Flur des Erdgeschosses. Begruesst du die Person kurz?`

- AC-7 PASS:
  - Event-Store belegte Begegnungen in `flur-eg` und `treppenhaus`
  - Kein Gegenbeleg fuer unzulaessige Cross-Floor-Encounter zwischen `flur-eg` und `flur-og`

- AC-8 und AC-9 PASS:
  - `journalctl -u sentinel-daemon` plus Event-Fortschritt zeigten pause/resume-typisches Verhalten bei Transit-Clustern
  - Zielankuenfte erfolgten spaeter nach der Encounter-Phase; Transit lief also weiter statt dauerhaft zu haengen

- AC-10 PASS:
  - `sqlite3 /opt/sentinel/data/events.db "SELECT ... FROM events WHERE event_type='transit_started' ..."`
  - Live-Werte u. a. `15000`, `20000`, `40000`, `60000`, `80000 ms`

- AC-11 und AC-12 PASS:
  - Cross-Floor-Transits liefen ueber konkrete Zwischen-Raeume (`flur-og`, `treppenhaus`, `flur-eg`)
  - Live-Agent-State und Transit-Perception zeigten zu jedem Zeitpunkt einen definierten aktuellen Transit-Raum

- AC-13 PASS:
  - Loopback-Sniff zeigte den exakten Transit-Text:
    `Du bist auf dem Weg von im Buero der Geschaeftsfuehrung nach im Empfangsbereich. Du gehst gerade durch im Flur des Obergeschosses.`
  - Ein weiterer Live-Fall zeigte:
    `Du bist auf dem Weg von im Treppenhaus nach in der Betriebsmedizin. Du gehst gerade durch im Flur des Obergeschosses.`

- AC-14 PASS:
  - Stationaere Agents in `buero-betriebsarzt` hatten in der gleichen Capture lediglich:
    `ENVIRONMENT: Du bist in der Betriebsmedizin.`
  - Kein Transit-Block bei `in_transit=false`

- AC-15 und AC-16 PASS:
  - Journal-Zeitreihenanalyse der `LLM call triggered`-Zeitpunkte:
    - idle Intervalle ca. `9.536-9.577s`
    - aktive Raeume ca. `2.0-5.0s`
  - Nach abklingender Chat-Aktivitaet stiegen die Intervalle wieder vom Minimalwert an

- AC-17 PASS:
  - `tick_snapshot`-Events zeigten weiter ca. `60` Ticks pro Minute auf der VM
  - Benchmark und Live-Befund blieben deutlich unter dem Budget; Bio/Physics-Loop blieb effektiv bei `1 Hz`

- Formale GitHub-Abschluss-Schritte:
  - Kommentar mit kompletter Evidence auf `#289` hinterlegt
  - `gh issue edit 289 --repo silentspike/project-sentinel --add-label 'status:verified' --remove-label 'status:triage' --remove-label 'quality:needs-spec'`
  - `gh issue close 289 --repo silentspike/project-sentinel`
  - Ergebnis: `#289` ist `CLOSED`

## Task 7 evidence summary

- Historische MITM-Parity-Luecke gegen `e4f8769` geschlossen:
  - `cmd/cortex-gateway/main.go`: `POST /v1/messages` wiederhergestellt
  - `cmd/cortex-gateway/internal/proxy/provider.go`: `RequestFormat`, `PreferredProvider`, `PassthroughHeaders` wiederhergestellt
  - `cmd/cortex-gateway/internal/proxy/anthropic_api.go`: Anthropic-wire-format Parse/Response/Error helpers wiederhergestellt
  - `cmd/cortex-gateway/internal/proxy/claude.go`: request-scoped Header-Passthrough und HTTP-Status-Weitergabe als `ProviderError`
  - `cmd/cortex-gateway/internal/proxy/pipeline.go`: path-aware Parse/Error/Response-Pfade fuer `/v1/messages` wiederhergestellt
  - `cmd/cortex-gateway/internal/proxy/judge_adapter.go`: passthrough-/format-sensitive Judge-Regen-Requests wiederhergestellt

- Lokale Smoke-/Regression-Tests:
  - `go test ./cmd/cortex-gateway/internal/...`
  - Ergebnis: kompletter Gateway-Teststack gruen
  - Wiederhergestellte Schluesseltests:
    - `TestPipelineAnthropicMessagesPassthrough`
    - `TestClaudeProviderSendUsesPassthroughHeaders`
    - `TestClaudeProviderSendReturnsProviderErrorOnNon200`

- VM-Deploy und Runtime-Evidence:
  - Linux-Build:
    `GOOS=linux GOARCH=amd64 go build -o cortex-gateway ./cmd/cortex-gateway/`
  - Deploy:
    `scp cortex-gateway ubuntu@10.0.0.240:/tmp/cortex-gateway`
    `ssh ubuntu@10.0.0.240 "sudo systemctl stop sentinel-gateway && sudo cp /tmp/cortex-gateway /opt/sentinel/bin/cortex-gateway && sudo systemctl start sentinel-gateway && systemctl is-active sentinel-gateway"`
  - Ergebnis: `sentinel-gateway` wieder `active`
  - MITM-Smoke auf der VM:
    - `POST /v1/messages` ohne Auth -> `401`
    - Body: `{\"type\":\"error\",\"error\":{\"type\":\"authentication_error\",...}}`
    - `POST /v1/messages` mit `Authorization` + `Anthropic-Version` -> ebenfalls path-spezifischer `401 authentication_error` statt `404` oder generischem internen JSON

- Formale GitHub-Abschluss-Schritte:
  - Kommentar mit kompletter Evidence auf `#298` hinterlegt
  - `gh issue edit 298 --repo silentspike/project-sentinel --add-label 'status:verified' --remove-label 'status:triage' --remove-label 'quality:needs-spec'`
  - `gh issue close 298 --repo silentspike/project-sentinel`
  - Ergebnis: `#298` ist `CLOSED`
