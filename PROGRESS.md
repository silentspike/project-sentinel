# PROGRESS

## Status

- Plan source: `/work/company/codex-review.md`
- Overall status: `IN_PROGRESS`
- Current task: `3. Neues Parity-Issue für origin/main gegen e4f8769/VM anlegen`
- Current branch: `feat/issue-289-room-phase2-closure`
- Pull policy: `Kein Pull von main in den aktuellen Branch ohne explizite User-Freigabe`
- Last refresh: `2026-04-03`

## Current findings

- GitHub `origin/main` steht auf `4e69d4f`, lokales `main` auf `e4f8769`, aktueller Arbeitsbranch auf `59813ff`.
- `e4f8769` und `e1cb7e6` enthalten den MITM-Vertrag mit `/v1/messages`, `PassthroughHeaders` und `applyAnthropicForwardHeaders(...)`.
- GitHub `origin/main` enthält diesen MITM-Vertrag aktuell nicht mehr.
- Die laufende VM verhält sich MITM-fähig: `POST /v1/messages` liefert `401`, nicht `404`.
- `#295` ist im aktuellen Code provider-unabhängig durch `baseGate`/`heard` abgesichert.
- `#288` ist jetzt formal geschlossen; `status:verified` ist gesetzt und der Close-Kommentar hält die getrennte Parity-Lücke fest.
- `#295` ist jetzt formal geschlossen; `status:verified` ist gesetzt und der Close-Kommentar verweist korrekt auf `baseGate`/`heard`.
- Die vorhandene `#289`-Arbeit auf diesem Branch bleibt erhalten und wird für den späteren `#289`-Task wiederverwendet.

## Blocked items

- Kein akuter Blocker für Task 1 bis Task 5.
- `#289`-Abschluss hängt weiterhin an vollständiger Runtime-Evidence für `17/17` ACs und Benchmarks.
- Parity-Fix gegen `origin/main` ist ein eigener späterer Task und kein Blocker für das formale Schließen von `#288`/`#295`.

## Commit references

- Vorhandene Branch-Basis für `#289`:
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
| 3 | Neues Parity-Issue für origin/main gegen e4f8769/VM anlegen | IN_PROGRESS | schmal geschnittenes GitHub-Issue mit präzisem Scope und Labels | command |
| 4 | #296 mit kurzem Parity-Hinweis kommentieren | TODO | knapper Hinweis auf Parity-Lücke, ohne Scope-Mix | command |
| 5 | Lokale Doku- und Memory-Artefakte nuanciert korrigieren | TODO | `AGENTS.md`, `agents.md`, `test-288-*` an neuen Stand anpassen | inspect, command |
| 6 | #289 gemäß bestehender Branch-/Progress-Basis vollständig verifizieren und abschließen | TODO | vorhandene Room-Phase-2-Arbeit final zu Ende führen, `17/17` ACs + Benchmarks + Close | command, system, inspect |
| 7 | Parity-Lücke zwischen e4f8769 und origin/main implementieren | TODO | MITM-Vertrag auf kanonischem `origin/main` wiederherstellen | command, system, inspect |
| 8 | Verbleibende #296-Follow-ups sauber weiterbearbeiten | TODO | Streaming/Blocks/Observability/Redaction nach Scope | command, system, inspect |
| 9 | Plan-Verifikation | TODO | `/work/company/codex-review.md` Zeile für Zeile gegen Ergebnis abgleichen | command, inspect, system |

## Task 1 evidence summary

- AC1.1 PASS
  - `gh issue view 288 --repo silentspike/project-sentinel --json number,title,state,labels,url`
  - Ergebnis vor Änderung: `state=OPEN`, Label `status:review` vorhanden.
- AC1.2 PASS
  - Belegbasis:
    - `git branch --contains e4f8769 --all`
    - `gh api repos/silentspike/project-sentinel/branches/main --jq '.commit.sha'`
    - `ssh ubuntu@10.0.0.240 "curl ... /v1/messages ..."`
  - Ergebnis: verifizierter MITM-Stand auf `e4f8769`/VM, getrennte Parity-Lücke zu `origin/main`.
- AC1.3 PASS
  - `gh issue edit 288 --repo silentspike/project-sentinel --add-label 'status:verified' --remove-label 'status:review'`
  - Nachweis:
    - `gh issue view 288 --repo silentspike/project-sentinel --json state,labels,comments --jq '{state:.state, labels:[.labels[].name]}'`
    - Ergebnis: `status:verified` vorhanden, `status:review` entfernt.
- AC1.4 PASS
  - `gh issue close 288 --repo silentspike/project-sentinel`
  - Nachweis:
    - `gh issue view 288 --repo silentspike/project-sentinel --json state --jq '.state'`
    - Ergebnis: `CLOSED`

## Current task pre-check

## Task 2 evidence summary

- AC2.1 PASS
  - `gh issue view 295 --repo silentspike/project-sentinel --json number,title,state,labels,url`
  - Ergebnis vor Änderung: `state=OPEN`.
- AC2.2 PASS
  - Struktureller Beleg:
    - `go test ./internal/synthesis/... -run TestBioHungerBlockedByHeardMetadata -v -count=1`
    - Ergebnis: `PASS`
    - `rules.go`: `baseGate = !HasHeard && !IsAddressed && !HasChaos && !HasImpulse`
    - `context.go`: `heard` wird in den Guard hochgezogen
  - Live-Beleg:
    - `curl -X POST localhost:8084/operator/chat ...`
    - `journalctl -u sentinel-daemon ...`
    - Ergebnis: mehrere `LLM call triggered ... has_heard=true`
    - Event-Store: echte `agent_action_received|...|Chat|...` Events für betroffene Agents
- AC2.3 PASS
  - `gh issue edit 295 --repo silentspike/project-sentinel --add-label 'status:verified'`
  - Nachweis:
    - `gh issue view 295 --repo silentspike/project-sentinel --json state,labels,comments ...`
    - Ergebnis: `status:verified` vorhanden.
- AC2.4 PASS
  - `gh issue close 295 --repo silentspike/project-sentinel`
  - Nachweis:
    - `gh issue view 295 --repo silentspike/project-sentinel --json state --jq '.state'`
    - Ergebnis: `CLOSED`

## Current task pre-check

### Task 3: Neues Parity-Issue für origin/main gegen e4f8769/VM anlegen

Was muss getan werden:
- den Parity-Scope präzise aus den verifizierten Branch-/VM-Fakten schneiden
- neues GitHub-Issue mit sauberen Labels und Acceptance formulieren
- Bezug zu `e4f8769`, `fix/issue-296-mitm-followups`, `origin/main` und VM sauber dokumentieren
- Ergebnis in `PROGRESS.md` dokumentieren

Welche ACs müssen für den Task passen:
- AC3.1: neues Issue ist erstellt und referenzierbar
- AC3.2: Body beschreibt den Scope als Parity-Lücke, nicht als Architektur-Widerruf
- AC3.3: Labels passen zum schmalen Bug-/Parity-Scope

Wie wird jede AC bewiesen:
- AC3.1: `gh issue create ...` plus `gh issue view <neu>`
- AC3.2: frischer `gh issue view <neu> --json body`
- AC3.3: `gh issue view <neu> --json labels`

Erwartete Dateiänderungen:
- `/work/company/project-sentinel/PROGRESS.md`

Bekannte Risiken oder Abhängigkeiten:
- Kein Repo-Code wird für Task 3 geändert.
- Das neue Issue darf weder `#288` noch `#296` entwerten; es muss nur die Parity-Lücke zwischen verifiziertem MITM-Stand und kanonischem `origin/main` festhalten.
