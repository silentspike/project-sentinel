---
id: SENTINEL-STATUS
status: Stable
ssot: true
refs:
  docs: [CLAUDE.md]
  affects: [.github/labels.yml, .github/workflows/issue-quality.yml]
---
# Statusmodell: implemented / deployed / verified

## TL;DR

- Issues durchlaufen: `triage` → `backlog` → `ready` → `in-progress` → `review` → `completed`
- Gate-Issues erfordern: implemented (PR merged) → deployed (VM live) → verified (smoke test)
- `status:completed` NUR mit Verify-Evidence (PR link, test output, screenshot)
- `status:partial` erfordert Folge-Issue mit `scope:partial`

## Label-Schema (42 Labels)

SSOT: `.github/labels.yml` (synced via EndBug/label-sync Workflow).

### Status-Labels (Kanban)

```
triage → backlog → ready → in-progress → review → completed
                                      ↘ blocked (temporaer)
                                              ↘ partial (scope:partial)
```

| Label | Bedeutung | Wer setzt es | Automatisiert |
|-------|-----------|-------------|---------------|
| `status:triage` | Neues Issue, braucht Bewertung | auto-label.yml | Ja |
| `status:backlog` | Akzeptiert, nicht gestartet | Entwickler | Nein |
| `status:ready` | Alle Dependencies erfuellt, startbar | Entwickler | Nein |
| `status:in-progress` | Aktiv in Bearbeitung | Entwickler | Nein |
| `status:review` | PR offen, in Review | Entwickler | Nein |
| `status:blocked` | Blockiert durch Dependency | Entwickler | Nein |
| `status:completed` | Arbeit abgeschlossen mit Evidence | Entwickler | Nein |
| `status:partial` | Teillieferung, Folge-Issue existiert | Entwickler | Nein |

### Pflicht-Kombinationen

Jedes Issue braucht MINDESTENS:

| Kategorie | Labels | Pflicht |
|-----------|--------|---------|
| Status | genau 1x `status:*` | Ja |
| Scope | genau 1x `scope:full\|partial\|experimental` | Ab `status:ready` |
| Quality | genau 1x `quality:ready\|needs-spec\|needs-evidence` | Ab `status:ready` |
| Type | genau 1x `type:*` | Ja |
| Size | genau 1x `size:S\|M\|L\|XL` | Ab `status:ready` |
| Priority | genau 1x `prio:*` | Ja |

### Quality-Gate Labels

| Label | Bedeutung | Voraussetzung |
|-------|-----------|---------------|
| `quality:needs-spec` | AC/Verify/Evidence fehlen | Issue erstellt |
| `quality:ready` | Spec + AC + Verify komplett | issue-quality.yml validiert |
| `quality:needs-evidence` | Implementiert, Evidence fehlt | PR merged |

## Gate-Issue Lifecycle

Gate-Issues (`gate:` Prefix) haben ein erweitertes Statusmodell:

```
ready → in-progress → implemented → deployed → verified → completed
```

| Phase | Bedeutung | Evidence |
|-------|-----------|----------|
| implemented | PR merged, Code auf main | PR link, CI green |
| deployed | Service laeuft auf Deploy-VM (192.0.2.240) | systemctl status, curl health |
| verified | Smoke-Test / E2E bestanden | Test-Output, Screenshot |
| completed | Alle 3 Phasen bestanden | Alle Evidence-Links |

**CRITICAL:** Ein Gate-Issue darf NICHT `status:completed` erhalten ohne Evidence fuer ALLE drei Phasen. `status:completed` ohne Verify-Evidence ist ein Governance-Verstoss.

## Uebergaenge und Regeln

### Vorwaerts-Uebergaenge

| Von | Nach | Bedingung |
|-----|------|-----------|
| triage | backlog | Issue bewertet, Type/Prio gesetzt |
| backlog | ready | Dependencies erfuellt, Scope/Quality/Size gesetzt |
| ready | in-progress | Entwickler beginnt Arbeit |
| in-progress | review | PR erstellt |
| review | completed | PR merged + Evidence dokumentiert |
| review | partial | Teillieferung, Folge-Issue erstellt |

### Sonder-Uebergaenge

| Von | Nach | Bedingung |
|-----|------|-----------|
| * | blocked | Dependency nicht erfuellt |
| blocked | ready | Dependency aufgeloest |
| completed | (closed) | Issue wird geschlossen (GitHub close) |
| partial | (closed) | Folge-Issue verlinkt |

### Verbotene Uebergaenge

- `triage` → `completed` (keine Arbeit uebersprungen)
- `backlog` → `completed` (Quality-Gate nicht passiert)
- `*` → `completed` ohne Evidence (AC-N1)
- `status:completed` + `quality:needs-spec` (Widerspruch)

## Automatisierte Gates

| Workflow | Prueft | Blockiert |
|----------|--------|-----------|
| `issue-quality.yml` | AC-Struktur, Verify, Evidence-Felder | `quality:ready` Promotion |
| `pr-quality.yml` | 7 PR-Sektionen, Linked Issues, AC Evidence | PR Merge |
| `pr-lint.yml` | Conventional Commits | PR Merge |
| `main-push-guard.yml` | Kein Direct Push auf main | Main-Branch |

## Label-Hygiene

### Bei Issue-Schliessung

1. Status-Label auf `status:completed` oder `status:partial` setzen
2. Bei `status:partial`: Folge-Issue verlinken
3. Evidence im letzten Kommentar oder PR Body dokumentieren

### Audit-Regel (AC-N1)

Kein geschlossenes Issue darf `status:completed` tragen OHNE:
- Mindestens 1 verlinkten PR (via `Closes #XX`)
- Evidence im PR Body (AC Mapping Tabelle)
- CI green auf dem PR

Historische Issues (Sprint 1-3, vor Governance-Einfuehrung) sind von dieser Regel ausgenommen — sie tragen `status:completed` als Legacy-Status.
