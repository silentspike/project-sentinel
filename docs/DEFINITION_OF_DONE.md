---
id: SENTINEL-DOD
status: Stable
ssot: true
refs:
  docs: [CLAUDE.md, STATUS_MODEL.md]
  affects: [.github/PULL_REQUEST_TEMPLATE.md]
---
# Definition of Done (DoD)

## TL;DR

- Feature/Fix: Code + Tests + CI green + Evidence + CHANGELOG
- Gate-Issue: implemented + deployed + verified (3 Phasen)
- `status:completed` NUR wenn ALLE Checkpunkte erfuellt

## Feature / Bug Fix DoD

Ein Feature oder Bug Fix ist DONE wenn:

### Code

- [ ] Implementierung vollstaendig (keine TODOs, keine Stubs bei `scope:full`)
- [ ] Bestehende Tests gruen (`cargo remote -- test --workspace` / `bun test`)
- [ ] Neue Tests fuer neue Funktionalitaet / Regression-Test fuer Bug Fix
- [ ] Clippy clean (`cargo remote -- clippy --workspace --all-targets -- -D warnings`)
- [ ] Format (`make fmt`)
- [ ] Keine Security-Vulnerabilities eingefuehrt

### PR

- [ ] Conventional Commit Message (`feat:`, `fix:`, `docs:`, etc.)
- [ ] PR Body mit 7 Pflicht-Sektionen:
  1. `## Summary`
  2. `## Changes`
  3. `## Linked Issues` (mit `Closes #XX`)
  4. `## Test Plan`
  5. `## Benchmarks` (oder N/A mit Begruendung)
  6. `## Evidence (AC Mapping)` (mit `| AC-<n> |` Tabelle)
  7. `## Checklist`
- [ ] CI green (alle relevanten Jobs: lint, test, gate, ci-pass)
- [ ] CHANGELOG.md aktualisiert

### Evidence

- [ ] Jedes AC hat Evidence (Command + Output oder Screenshot)
- [ ] Honest Confidence Level dokumentiert
- [ ] NOT Tested Bereiche explizit benannt

### Labels (bei Schliessung)

- [ ] `status:completed` gesetzt
- [ ] `quality:ready` vorhanden
- [ ] `scope:full` oder `scope:partial` (mit Folge-Issue bei partial)

## Gate-Issue DoD

Gate-Issues erfordern drei Phasen. ALLE muessen dokumentiert sein:

### Phase 1: Implemented

- [ ] PR(s) merged auf main
- [ ] CI green auf main
- [ ] Code-Review abgeschlossen

### Phase 2: Deployed

- [ ] Service auf Deploy-VM (10.0.0.240) gestartet
- [ ] Health-Endpoint erreichbar (`curl localhost:PORT/health`)
- [ ] systemd Service aktiv (`systemctl status sentinel-*`)

### Phase 3: Verified

- [ ] Smoke-Test bestanden (`make smoke-test` oder manuell)
- [ ] Funktionale Verifikation der ACs auf der VM
- [ ] Evidence im Issue/PR dokumentiert

### Gate-Schliessung

- [ ] Alle 3 Phasen mit Evidence belegt
- [ ] `status:completed` gesetzt
- [ ] Issue geschlossen

## Docs-Issue DoD

- [ ] Dokumentation geschrieben und referenzierbar
- [ ] Konsistenz mit bestehenden Docs geprueft
- [ ] Pfad/Link im Issue dokumentiert
- [ ] CHANGELOG.md aktualisiert

## Anti-Pattern: Falsche Completion

Folgendes ist ein Governance-Verstoss:

| Anti-Pattern | Warum falsch |
|-------------|-------------|
| `status:completed` ohne PR | Keine nachvollziehbare Aenderung |
| `status:completed` + `quality:needs-spec` | Spec nie erfuellt |
| Gate `completed` ohne Deploy-Evidence | Nur implementiert, nicht verifiziert |
| `status:completed` + offene TODO-Kommentare im Code | Nicht fertig |
| `scope:full` + Stubs/Mocks im Produktionspfad | Nicht production-ready |
