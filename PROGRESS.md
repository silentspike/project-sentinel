# PROGRESS

## Status

- Issue: `#289` Room-Kommunikation Phase 2
- Overall status: `IN_PROGRESS`
- Current task: `2. GitHub-AC-Matrix erstellen`
- Plan source: `/work/company/codex-plan289.md`
- GitHub SSOT: `gh issue view 289 --repo silentspike/project-sentinel`
- Last refresh: `2026-04-03`

## Current findings

- GitHub Issue `#289` ist `OPEN` und weiterhin mit `status:triage` gelabelt.
- Aktueller Repo-Branch ist jetzt korrekt `feat/issue-289-room-phase2-closure` von `origin/main`.
- Der Worktree ist nicht sauber; aktuell sichtbar sind untracked Dateien: `AGENTS.md`, `hooks/`, `test-288-matrix-v3.md`, `test-288-matrix.md`, `test-288-results.md`, `test-288-verification.md`.
- Die genehmigte Planbasis fuer `#289` ist `/work/company/codex-plan289.md`.
- Die lokale Handover-Datei bestaetigt den IST-Zustand `22/28` PASS, `6/28` BLOCKED (`MO1-MO6`) und verweist auf Operator-API als Trigger-Pfad.
- `mainrag` ist derzeit nicht verfuegbar (`localhost:3001` connection refused). Das blockiert die Ausfuehrung nicht, ist aber als Umgebungsbefund festgehalten.

## Blocked items

- `MO1-MO6` sind historisch als Runtime-Trigger-Problem bekannt und muessen im laufenden System reproduzierbar gemacht werden.

## Task table

| # | Task | Status | Scope | Evidence |
|---|------|--------|-------|----------|
| 1 | Baseline bestaetigen | DONE | Commit-Basis, Issue-SSOT, Branch/Worktree, Runtime-Status | command |
| 2 | GitHub-AC-Matrix erstellen | IN_PROGRESS | 17 ACs in pruefbare Matrix mit Evidence-Mapping ueberfuehren | command, inspect |
| 3 | MO1-MO6 im laufenden System reproduzierbar machen | TODO | reproduzierbare Operator-/API-Trigger fuer Kapazitaetstests | command, system |
| 4 | Verbleibende Code-Luecken schliessen | TODO | nur die real offenen Ursachen beheben | command, inspect, system |
| 5 | Benchmarks implementieren oder an vorhandene Harnesses anbinden | TODO | BFS-, Encounter- und Tick-Benchmarkpfade absichern | command |
| 6 | TOGAF aktualisieren | TODO | Transit-Zeiten auf `15s-120s` angleichen | inspect, command |
| 7 | 17/17 ACs mit frischer Evidence verifizieren | TODO | jede AC einzeln im laufenden System oder passendem Harness nachweisen | command, system, inspect |
| 8 | Abschlussartefakt erstellen | TODO | AC-Endstatus, Benchmarks, Risiken, Close-Empfehlung dokumentieren | inspect, command |
| 9 | GitHub-Issue formal schliessen | TODO | `status:verified` setzen, `status:triage` entfernen, Issue schliessen | command |
| 10 | Plan-Verifikation | TODO | Plan komplett rereaden und Ergebnis Zeile fuer Zeile abgleichen | command, inspect, system |

## Detailed tasks

### 1. Baseline bestaetigen

Scope:
- bestaetigen, was aus `#289` bereits implementiert ist
- GitHub-Issue, lokale Handover-Infos und Branch-/Worktree-Zustand auf einen Stand bringen
- offene Risiken fuer die eigentliche Ausfuehrung sichtbar machen

Checklist:
- GitHub Issue `#289` live abrufen
- relevante Handover-/Memory-Eintraege fuer `#289` rereaden
- aktuellen Branch und Worktree dokumentieren
- Commit-Basis `6750e24` plus Folgefixes gegen Repo-History bestaetigen
- offenen Runtime-Befund `22/28`, `MO1-MO6` blockiert festhalten

Acceptance criteria:
- AC1.1: GitHub Issue `#289` live bestaetigt
  Evidence: `command`
- AC1.2: lokaler IST-Zustand und Handover-Befund dokumentiert
  Evidence: `command`
- AC1.3: Branch-/Worktree-Risiken transparent festgehalten
  Evidence: `command`
- AC1.4: Commit-Baseline fuer Room Phase 2 bestaetigt
  Evidence: `command`

### 2. GitHub-AC-Matrix erstellen

Scope:
- alle `17` GitHub-ACs in eine operative Matrix ueberfuehren
- pro AC Trigger, Befehl, erwartetes Signal und Evidence-Typ festlegen

Checklist:
- AC-Text aus Issue extrahieren
- ACs nach Feature-Clustern gruppieren
- pro AC den Nachweisweg definieren
- Matrix auf VM-/Repo-/Benchmark-Nachweise abbilden

Acceptance criteria:
- AC2.1: alle `17` ACs einzeln in `PROGRESS.md` oder Artefakt erfasst
  Evidence: `inspect`
- AC2.2: jeder AC hat genau einen primaeren Nachweisweg
  Evidence: `inspect`
- AC2.3: kein AC bleibt ohne Evidence-Typ
  Evidence: `inspect`

### 3. MO1-MO6 im laufenden System reproduzierbar machen

Scope:
- bestaetigen, wie sich Kapazitaets-ACs gezielt im laufenden System ausloesen lassen
- vorhandene Operator-Pfade nutzen, nicht neue Test-Sonderpfade erfinden

Checklist:
- bestehende Runtime-Trigger (`/operator/chat`, `/operator/gaia`, API) pruefen
- vollen Raum und Transit-Faelle reproduzierbar herstellen
- minimalen Trigger-Fix implementieren, falls der bestehende Pfad nicht reicht
- Repro-Schritte dokumentieren

Acceptance criteria:
- AC3.1: MO1/MO2 sind im VM-System gezielt provozierbar
  Evidence: `system`
- AC3.2: MO3/MO6 sind im VM-System gezielt provozierbar
  Evidence: `system`
- AC3.3: Repro-Schritte sind dokumentiert und wiederholbar
  Evidence: `command`

### 4. Verbleibende Code-Luecken schliessen

Scope:
- nur die real noch offenen Defekte oder Trigger-Luecken beheben
- kein breitflaechiges Refactoring

Checklist:
- Root-Cause pro offenem Befund isolieren
- betroffene Rust/Go/Config-Dateien lesen
- gezielte Aenderungen implementieren
- Regression gegen Room/Transit/Heartbeat/Gateway-Pfade pruefen

Acceptance criteria:
- AC4.1: alle fuer offene Befunde geaenderten Dateien sind begruendbar
  Evidence: `inspect`
- AC4.2: keine planfremde Refactor-Welle eingefuehrt
  Evidence: `inspect`
- AC4.3: notwendige Regressionstests oder Runtime-Checks laufen gruen
  Evidence: `command`

### 5. Benchmarks implementieren oder an vorhandene Harnesses anbinden

Scope:
- die drei Pflicht-Benchmarks messbar und wiederholbar machen

Checklist:
- vorhandene Benchmark-Harnesses finden
- BFS-Benchmark anbinden oder erstellen
- Encounter-Benchmark anbinden oder erstellen
- Tick-Duration-Messung vorher/nachher definieren

Acceptance criteria:
- AC5.1: Route-BFS Messpfad vorhanden und ausfuehrbar
  Evidence: `command`
- AC5.2: Encounter Detection Messpfad vorhanden und ausfuehrbar
  Evidence: `command`
- AC5.3: Tick-Duration Vorher/Nachher sauber messbar
  Evidence: `command`

### 6. TOGAF aktualisieren

Scope:
- alte Transit-Timing-Angabe in TOGAF auf die Issue-Realitaet angleichen

Checklist:
- alte `2-5 Min` Stelle lokalisieren
- neue `15s-120s` Angabe einpflegen
- Begruendung auf Gebaeudeanalyse/Issue referenzieren

Acceptance criteria:
- AC6.1: TOGAF enthaelt keine widerspruechliche Transit-Zeit mehr
  Evidence: `inspect`
- AC6.2: neue Transit-Zeit ist sichtbar auf `15s-120s` gesetzt
  Evidence: `inspect`
- AC6.3: Begruendung oder Kontextverweis ist vorhanden
  Evidence: `inspect`

### 7. 17/17 ACs mit frischer Evidence verifizieren

Scope:
- jede GitHub-AC einzeln nachweisen
- Runtime truth ist primaer, Repo-/Code-Evidence nur wo strukturell noetig

Checklist:
- AC-1 bis AC-17 einzeln triggern
- pro AC frische Evidence sichern
- Fehlfaelle bis zu 5-mal ernsthaft nachfixen und erneut pruefen
- Blocker separat dokumentieren, falls eine AC nicht passierbar ist

Acceptance criteria:
- AC7.1: `AC-1` bis `AC-4` je separat belegt
  Evidence: `system`
- AC7.2: `AC-5` bis `AC-9` je separat belegt
  Evidence: `system`
- AC7.3: `AC-10` bis `AC-14` je separat belegt
  Evidence: `system`
- AC7.4: `AC-15` bis `AC-17` je separat belegt
  Evidence: `system`

### 8. Abschlussartefakt erstellen

Scope:
- die komplette Close-Evidence fuer Mensch und GitHub vorbereiten

Checklist:
- AC-Endstatus sammeln
- Benchmark-Werte mit Ziel/Ist dokumentieren
- verbleibende Risiken oder Restarbeiten auflisten
- Close-Empfehlung vorbereiten

Acceptance criteria:
- AC8.1: Artefakt deckt alle `17` ACs ab
  Evidence: `inspect`
- AC8.2: Benchmarks stehen mit Ziel/Ist im Artefakt
  Evidence: `inspect`
- AC8.3: Risiken/Blocker sind explizit genannt
  Evidence: `inspect`

### 9. GitHub-Issue formal schliessen

Scope:
- formale GitHub-Schritte erst nach kompletter Verifikation

Checklist:
- `status:verified` setzen
- `status:triage` entfernen
- Issue schliessen

Acceptance criteria:
- AC9.1: `status:verified` gesetzt
  Evidence: `command`
- AC9.2: `status:triage` entfernt
  Evidence: `command`
- AC9.3: Issue `#289` geschlossen
  Evidence: `command`

### 10. Plan-Verifikation

Scope:
- komplette Ruecklese gegen `/work/company/codex-plan289.md`
- keinen Teil stillschweigend auslassen

Checklist:
- Plan vollstaendig rereaden
- jede Planzeile gegen Umsetzung und Evidence abgleichen
- Mismatches sofort als Finding behandeln
- finalen Status auf `COMPLETE` oder `BLOCKED` setzen

Acceptance criteria:
- AC10.1: Plan vollstaendig gegen Ergebnis abgeglichen
  Evidence: `inspect`
- AC10.2: alle Abweichungen sind entweder gefixt oder als Blocker dokumentiert
  Evidence: `inspect`
- AC10.3: finaler Repo-Status und `PROGRESS.md` konsistent
  Evidence: `command`

## Commit references

- none yet
