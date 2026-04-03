# PROGRESS

## Status

- Plan source: `/work/company/codex-review.md`
- Overall status: `IN_PROGRESS`
- Current task: `8. Verbleibende #296-Follow-ups sauber weiterbearbeiten`
- Current branch: `feat/issue-289-room-phase2-closure`
- Pull policy: `Kein Pull von main in den aktuellen Branch ohne explizite User-Freigabe`
- Last refresh: `2026-04-03`

## Current findings

- GitHub `origin/main` stand zu Task-Start auf `4e69d4f`; der historische MITM-Vertrag aus `e4f8769` ist jetzt wieder im aktuellen Gateway-Code und auf der VM deployed.
- `#288` ist formal geschlossen; `status:verified` ist gesetzt und der Close-Kommentar grenzt die getrennte Parity-Luecke sauber ab.
- `#295` ist formal geschlossen; `status:verified` ist gesetzt und der Close-Kommentar verweist korrekt auf den provider-unabhaengigen `baseGate`-/`heard`-Fix.
- `#296` bleibt offen als echtes Follow-up fuer Dashboard/Streaming/Blocks/Observability/Redaction; die Parity-Luecke liegt separat in `#298`.
- `#289` ist jetzt formal geschlossen; `status:verified` ist gesetzt, `status:triage` und `quality:needs-spec` sind entfernt.
- `#298` ist jetzt formal geschlossen; `/v1/messages`, request-scoped Anthropic-Passthrough, path-spezifische Anthropic-Responses und die zugehoerigen Smoke-Tests sind wiederhergestellt.
- TOGAF und lokale Artefakte sind auf den verifizierten Room-Phase-2-Stand angeglichen: realistische Transit-Zeiten `15s-120s`, Transit-Perception mit Zwischen-Raum und adaptiver Heartbeat ohne separaten Async-Task.

## Blocked items

- Kein aktiver Blocker mehr fuer `#289`.
- Naechster aktiver Planpunkt sind die verbleibenden `#296`-Follow-ups.

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
| 1 | #288 formal mit verifizierter Begründung schließen | DONE | Issue-Status prüfen, `status:verified` setzen, korrekt kommentieren, schließen | command |
| 2 | #295 formal mit baseGate/heard-Begründung schließen | DONE | Issue-Kommentar mit Code-/Testbasis, `status:verified` setzen, schließen | command, inspect |
| 3 | Neues Parity-Issue für origin/main gegen e4f8769/VM anlegen | DONE | schmal geschnittenes GitHub-Issue mit präzisem Scope und Labels | command |
| 4 | #296 mit kurzem Parity-Hinweis kommentieren | DONE | knapper Hinweis auf Parity-Lücke, ohne Scope-Mix | command |
| 5 | Lokale Doku- und Memory-Artefakte nuanciert korrigieren | DONE | `AGENTS.md`, `agents.md`, `test-288-*` an neuen Stand anpassen | inspect, command |
| 6 | #289 gemäß bestehender Branch-/Progress-Basis vollständig verifizieren und abschließen | DONE | vorhandene Room-Phase-2-Arbeit final zu Ende führen, `17/17` ACs + Benchmarks + Close | command, system, inspect |
| 7 | Parity-Lücke zwischen e4f8769 und origin/main implementieren | DONE | MITM-Vertrag auf kanonischem `origin/main` wiederherstellen | command, system, inspect |
| 8 | Verbleibende #296-Follow-ups sauber weiterbearbeiten | IN_PROGRESS | Streaming/Blocks/Observability/Redaction nach Scope | command, system, inspect |
| 9 | Plan-Verifikation | TODO | `/work/company/codex-review.md` Zeile für Zeile gegen Ergebnis abgleichen | command, inspect, system |

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
