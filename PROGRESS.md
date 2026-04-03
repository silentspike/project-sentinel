# PROGRESS

## Status

- Plan source: `/work/company/codex-review.md`
- Overall status: `IN_PROGRESS`
- Current task: `6. #289 gemaess bestehender Branch-/Progress-Basis vollstaendig verifizieren und abschliessen`
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
- Das neue Parity-Issue ist als `#298` offen und bildet die Lücke zwischen verifiziertem MITM-Stand und `origin/main` separat ab.
- `#296` hat jetzt einen knappen Parity-Hinweis mit Verweis auf `#298`, ohne den Follow-up-Scope von `#296` umzudefinieren.
- `AGENTS.md`, `agents.md` und die `test-288-*`-Artefakte unterscheiden jetzt sauber zwischen verifiziertem MITM-Stand und der offenen `origin/main`-Parity-Luecke.
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
| 3 | Neues Parity-Issue für origin/main gegen e4f8769/VM anlegen | DONE | schmal geschnittenes GitHub-Issue mit präzisem Scope und Labels | command |
| 4 | #296 mit kurzem Parity-Hinweis kommentieren | DONE | knapper Hinweis auf Parity-Lücke, ohne Scope-Mix | command |
| 5 | Lokale Doku- und Memory-Artefakte nuanciert korrigieren | DONE | `AGENTS.md`, `agents.md`, `test-288-*` an neuen Stand anpassen | inspect, command |
| 6 | #289 gemäß bestehender Branch-/Progress-Basis vollständig verifizieren und abschließen | IN_PROGRESS | vorhandene Room-Phase-2-Arbeit final zu Ende führen, `17/17` ACs + Benchmarks + Close | command, system, inspect |
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

## Task 3 evidence summary

- AC3.1 PASS
  - `gh issue create ...`
  - Ergebnis: neues Issue `#298` erstellt und referenzierbar.
- AC3.2 PASS
  - `gh issue view 298 --repo silentspike/project-sentinel --json body`
  - Ergebnis: Scope ist ausdrücklich Parity-Lücke, nicht Architektur-Widerruf.
- AC3.3 PASS
  - `gh issue view 298 --repo silentspike/project-sentinel --json labels`
  - Ergebnis: Labels `type:bug`, `status:triage`, `comp:cortex`, `comp:inference`, `prio:high`, `size:M`, `quality:needs-spec`.

## Current task pre-check

## Task 4 evidence summary

- AC4.1 PASS
  - `gh issue view 296 --repo silentspike/project-sentinel --json number,title,state,labels,url`
  - Ergebnis: `#296` ist weiterhin `OPEN` und referenzierbar.
- AC4.2 PASS
  - `gh issue comment 296 --repo silentspike/project-sentinel --body ...`
  - Ergebnis: Kommentar hält `#296` beim Follow-up-Scope und verweist die Parity-Lücke sauber auf `#298`.
- AC4.3 PASS
  - `gh issue view 296 --repo silentspike/project-sentinel --json comments --jq '.comments[-1].body'`
  - Ergebnis: der neue Kommentar ist sichtbar.

## Current task pre-check

## Task 5 evidence summary

- AC5.1 PASS
  - `/work/company/AGENTS.md` aktualisiert:
    - `#288`/`#295` jetzt als formal geschlossen
    - `#296` weiter offen
    - `#298` als Parity-Bug ergänzt
    - Gateway-Abschnitt präzisiert auf verifizierten MITM-Stand vs. `origin/main`
- AC5.2 PASS
  - `/work/company/agents.md` aktualisiert:
    - `#288`/`#295` auf `CLOSED`
    - `#296` nicht mehr als „formal close“
    - `#298` ergänzt
    - MITM-/Provider-Abschnitte nuanciert statt pauschal
- AC5.3 PASS
  - `test-288-matrix.md`, `test-288-matrix-v3.md`, `test-288-results.md` mit Parity-Hinweis versehen
  - Negativcheck:
    - `rg -n 'functionally complete, needs formal GitHub close workflow|effectively obsolete after `#288`|obsolet durch #288 anthropic-direct Architektur|status:verified Label \\\\+ `gh issue close`|GitHub `origin/main` heute bereits sicher denselben MITM-Stand' ...`
    - Ergebnis: keine Treffer

## Current task pre-check

### Task 6: #289 gemaess bestehender Branch-/Progress-Basis vollstaendig verifizieren und abschliessen

Was muss getan werden:
- vorhandene `#289`-Branch-Arbeit und vorhandene Runtime-Befunde wieder aufnehmen
- `17/17` ACs mit frischer, task-eigener Evidence fertigziehen
- Benchmarks, TOGAF-Hinweis, Abschlussartefakt und GitHub-Close durchziehen
- Ergebnis in `PROGRESS.md` dokumentieren

Welche ACs müssen für den Task passen:
- AC6.1: alle `17` GitHub-ACs von `#289` haben frische Evidence
- AC6.2: Pflicht-Benchmarks sind dokumentiert und innerhalb der Budgets
- AC6.3: `#289` ist nur bei vollständiger Evidence mit `status:verified` schließbar

Wie wird jede AC bewiesen:
- AC6.1: VM-Kommandos, Event-Store, Logs, API-State, gezielte Repo-Inspect wo strukturell nötig
- AC6.2: Benchmark-Commands plus Resultate
- AC6.3: `gh issue view/edit/close` erst nach vollständigem AC-Mapping

Erwartete Dateiänderungen:
- `/work/company/project-sentinel/PROGRESS.md`
- weitere `#289`-Artefakte je nach verbleibendem Bedarf

Bekannte Risiken oder Abhängigkeiten:
- Task 6 ist der erste wieder grosse Runtime-Block und kann an Live-Systemverhalten, Quotas oder fehlender Room-Phase-2-Evidence haengen.
- Die fruehere `#289`-Evidence auf diesem Branch ist Ausgangslage, ersetzt aber nicht automatisch die frische Abschluss-Evidence dieses Tasks.
