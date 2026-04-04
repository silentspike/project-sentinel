# PROGRESS

## Status

- Plan source: `User-Freigabe 2026-04-04: codex-security.md nach $start umsetzen`
- Overall status: `IN_PROGRESS`
- Current task: `Task 2 - #291 rustls-webpki 0.102.8 / async-nats-Pfad beheben`
- Current branch: `feat/security-291-292-293`
- Hook status: `PreToolUse TaskUpdate + PostToolUse start-enforcer projektlokal registriert`
- Last refresh: `2026-04-04 11:01 UTC / Task 1 abgeschlossen`

## Current findings

- `#292` ist auf aktuellem `main` re-verifiziert und geschlossen: `Cargo.lock` enthält kein `rustls-webpki 0.103.9`, `cargo remote -c -- tree -i rustls-webpki@0.103.9` findet keinen Paketpfad mehr, `status:verified` ist gesetzt.
- `#291` bleibt nach Plan der echte offene `rustls-webpki`-Rest über `async-nats` im Daemon-Pfad.
- `#293` bleibt ein migrationskritischer `bincode`-Themenblock und ist nicht als bloßer Version-Bump zu behandeln.

## Blocked items

- Keine harten Blocker beim Start.

## Commit references

- `TBD` `Task [1]: #292 re-verifizieren und formal schließen`

## Task table

| # | Task | Status | Scope | Evidence |
|---|------|--------|-------|----------|
| 1 | `#292` re-verifizieren und formal schließen | DONE | `rustls-webpki 0.103.9` auf aktuellem `main` gegen Lockfile, Dependency-Graph und Audit neu belegen; dann GitHub-Close-Workflow ausführen | inspect, command |
| 2 | `#291` `rustls-webpki 0.102.8` / `async-nats`-Pfad beheben | IN_PROGRESS | minimalen belastbaren Upgrade-Pfad implementieren, remote testen, auf VM deployen und NATS-/Daemon-Verhalten live verifizieren | inspect, command, system |
| 3 | `#293` `bincode 1.3.3` ablösen | TODO | Snapshot-/Persistenzpfade auf gepflegte Alternative migrieren, kompatibel testen und live Restore verifizieren | inspect, command, system |
| 4 | Plan-Verifikation | TODO | alle drei Security-Issues gegen Repo-, GitHub- und VM-Endstand vollständig gegenprüfen | inspect, command, system |

## Task details

### Task 1 - `#292` re-verifizieren und formal schließen

- Scope:
  - Lockfile und Dependency-Graph auf `rustls-webpki 0.103.9` prüfen
  - frische Security-Evidence sammeln
  - GitHub-Issue `#292` mit `status:verified` schließen
- Checklist:
  - `Cargo.lock` auf `0.103.9` vs `0.103.10` prüfen
  - Dependency-Graph mit `cargo remote -c -- tree` prüfen
  - wenn sinnvoll Audit-/Advisory-Evidence ergänzen
  - Kommentar im Issue mit Evidence posten
  - Label setzen und Issue schließen
- Acceptance criteria:
  - AC-1: `rustls-webpki 0.103.9` kommt im aktuellen Repo-Stand nicht mehr vor
  - AC-2: `rustls-webpki 0.103.10` ist der aktive gepflegte Pfad
  - AC-3: `#292` ist mit frischer Evidence und `status:verified` geschlossen
- Evidence plan:
  - AC-1 via `rg`/`Cargo.lock` und `cargo remote -c -- tree`
  - AC-2 via `cargo remote -c -- tree -i rustls-webpki@0.103.10`
  - AC-3 via GitHub-Issue-Kommentar, Label und Closed-State
- Outcome:
  - `#292` ist formal geschlossen und mit frischer Evidence kommentiert.
  - `Cargo.lock` zeigt nur noch `rustls-webpki 0.102.8` und `0.103.10`; `0.103.9` ist aus dem aktiven Repo-Stand verschwunden.
  - Der verbliebene `rustls-webpki`-Rest wurde sauber auf `#291` eingegrenzt.
- Evidence:
  - AC-1 PASS:
    - `rg -n "rustls-webpki" Cargo.lock` => nur `0.102.8` und `0.103.10`
    - `cargo remote -c -- tree -i rustls-webpki@0.103.9` => `did not match any packages`
  - AC-2 PASS:
    - `cargo remote -c -- tree -i rustls-webpki@0.103.10` => aktiver Pfad über `rustls 0.23.37`, `reqwest 0.12.28`, `quinn 0.11.9`, `zenoh-*` und `sentinel-daemon`
  - AC-3 PASS:
    - Kommentar: `issuecomment-4186940547`
    - `gh issue edit 292 --add-label status:verified`
    - `gh issue close 292`

### Task 2 - `#291` `rustls-webpki 0.102.8` / `async-nats`-Pfad beheben

- Scope:
  - minimalen Upgrade-Korridor fuer `async-nats` finden
  - verbleibenden `rustls-webpki 0.102.8`-Pfad aus dem aktiven Graph entfernen
  - Runtime-Impact auf Daemon/NATS live verifizieren
- Acceptance criteria:
  - AC-1: `cargo remote -c -- tree -i rustls-webpki@0.102.8` zeigt keinen aktiven Produktivpfad mehr
  - AC-2: relevante Rust-Tests und Clippy sind gruen
  - AC-3: Daemon startet und der NATS-/Bridge-Pfad bleibt auf `10.0.0.240` intakt

### Task 3 - `#293` `bincode 1.3.3` ablösen

- Scope:
  - Serialisierungs-/Snapshot-Grenzen explizit aufnehmen
  - Migrationsstrategie `dual-read / new-write` oder gleichwertig implementieren
  - Restore/Snapshot live verifizieren
- Acceptance criteria:
  - AC-1: `bincode 1.3.3` ist nicht mehr im aktiven Graph
  - AC-2: Snapshot-Erzeugung und -Restore funktionieren im Test
  - AC-3: der Daemon liest/schreibt Snapshots auf der VM ohne Regression

### Task 4 - Plan-Verifikation

- Scope:
  - Security-Issues, Commits, Tests, Deploys und VM-Evidence gegen den Endstand abgleichen
  - verbleibende Blocker oder Restrisiken klar dokumentieren
