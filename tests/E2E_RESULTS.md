# E2E Test Results — Project Sentinel
**Datum:** 2026-02-21
**VM:** ubuntu@10.0.0.240
**HEAD:** 5f6af0b01579b80ee75262ca9fdabc5f1eecc79b
**Testmethode:** Playwright headful + SSH Remote Commands

---

## T1: Infrastructure & Services

| Test | Ergebnis | Evidence |
|------|----------|----------|
| T1.1 sentinel-daemon active | PASS | systemctl is-active: active |
| T1.2 sentinel-cortex active | PASS | systemctl is-active: active |
| T1.3 sentinel-dashboard active | PASS | systemctl is-active: active |
| T1.4 sentinel-projection active | PASS | systemctl is-active: active |
| T1.5 sentinel-nats-bridge active | PASS | systemctl is-active: active |
| T1.6 sentinel-judge active | PASS | systemctl is-active: active |
| T1.7 nats-server active | PASS | systemctl is-active: active |
| T1.8 Cortex Proxy :8080/health | PASS | {"status":"ok","version":"0.1.0"} |
| T1.9 Cortex Control :8081/health | PASS | {"status":"ok"} |
| T1.10 Judge :8082/health | PASS | {"status":"ok","service":"sentinel-judge"} |
| T1.11 Bridge :8083/health | PASS | {"status":"ok","service":"sentinel-nats-bridge"} |
| T1.12 Dashboard :8000/api/health | PASS | {"status":"ok","uptime":418,"projection_lag":0} |
| T1.13 Cortex Proxy :8080/ready | PASS | {"ready":true} |
| T1.14 Judge :8082/ready | PASS | {"ready":true} |
| T1.15 Bridge :8083/ready | PASS | OK |
| T1.16 DEPLOYED_SHA matches HEAD | PASS | 5f6af0b == 5f6af0b |

**Result: 16/16 PASS**

---

## T2: Dashboard UI (Playwright headful)

| Test | Ergebnis | Screenshot |
|------|----------|------------|
| T2.1 Page loads with title "Project Sentinel - Dashboard" | PASS | e2e-T1-agents.png |
| T2.2 5 Nav Buttons visible (Agents, Bueroplan, Aktivitaet, Metriken, Cockpit) | PASS | Snapshot |
| T2.3 WebSocket verbunden ("Verbunden" gruen) | PASS | e2e-T1-agents.png |
| T2.4 Lag: 0 (gruen) | PASS | e2e-T1-agents.png |
| T2.5 Agents View: leer (korrekt, 0 aktive Agents) | PASS | e2e-T1-agents.png |
| T2.6 Bueroplan: 15 Raeume, 3 Etagen | PASS | e2e-T2-bueroplan.png |
| T2.7 Bueroplan: Raum-Typen korrekt (OFFICE, TRANSIT, MEETING, BREAK, BATHROOM, COMMON) | PASS | e2e-T2-bueroplan.png |
| T2.8 Metriken: 6 KPI-Kacheln sichtbar | PASS | e2e-T3-metriken.png |
| T2.9 Metriken: Chaos Events > 0 (23) | PASS | e2e-T3-metriken.png |
| T2.10 Aktivitaet: "Keine Aktivitaeten vorhanden" | PASS | e2e-T5-aktivitaet.png |
| T2.11 Cockpit: SLO-Status sichtbar | PASS | e2e-T4-cockpit.png |
| T2.12 Cockpit: Incidents listed (200, limited) | PASS | e2e-T4-cockpit.png |
| T2.13 Cockpit: Chaos SLO Violation detected (3483/3) | PASS | e2e-T4-cockpit.png |
| T2.14 Navigation: alle 5 Views wechselbar | PASS | Alle Screenshots |
| T2.15 Dashboard CPU < 10% (actual: 4.3%) | PASS | ps output |

**Result: 15/15 PASS**

---

## T15: Release Manifest

| Test | Ergebnis | Evidence |
|------|----------|----------|
| T15.1 Manifest has 31 artifacts | PASS | len(artifacts) = 31 |
| T15.2 Manifest git_sha matches DEPLOYED_SHA | PASS | Both = 5f6af0b... |
| T15.3 All 31 artifact SHA256 hashes match | PASS | 31/31 verified |

**Result: 3/3 PASS**

---

## T20a: Projection Worker

| Test | Ergebnis | Evidence |
|------|----------|----------|
| T20a.1 agent_live_view table exists | PASS | 0 rows (korrekt, keine Agents) |
| T20a.2 room_live_view = 15 rows | PASS | 15 rows |
| T20a.3 kpi_1m growing | PASS | 3731 rows (wachsend) |
| T20a.4 projection_lag = 0 | PASS | /api/health: projection_lag: 0 |

**Result: 4/4 PASS**

---

## T20b: Outbox Drain

| Test | Ergebnis | Evidence |
|------|----------|----------|
| T20b.1 pending = 0 | PASS | 0 pending |
| T20b.2 published > 200k | PASS | 220407 published |
| T20b.3 failed = 0 | PASS | 0 failed |
| T20b.4 retry_count column exists | PASS | Schema verified |
| T20b.5 last_error column exists | PASS | Schema verified |

**Result: 5/5 PASS**

---

## Firewall (Infrastruktur-Fix waehrend E2E)

UFW-Regeln fuer LAN-Zugriff (10.0.0.0/24) hinzugefuegt:
- Port 8000 (Dashboard), 8080 (Cortex Proxy), 8081 (Cortex Control)
- Port 8082 (Judge), 8083 (Bridge), 8222 (NATS Monitor)

---

## Bugs gefunden und gefixt waehrend E2E

### BUG-1: Dashboard 100% CPU (CRITICAL)
**Root Cause:** Fehlender Index auf `events.causation_id`. Cockpit-Endpoint
fuehrt N+1 Query fuer 78.000+ Events aus — jede Causation-Query dauert 24ms
(Full-Table-Scan auf 219k Events).
**Fix:**
1. `CREATE INDEX idx_events_causation ON events(causation_id)` — Query von 24ms auf 0.01ms
2. SQL LIMIT 200 fuer Incident-Queries im Cockpit-Endpoint
3. Index in Rust (sentinel-limbo) und Go (eventstore) Schema hinzugefuegt

### BUG-2: WebSocket-Upgrade fehlte (HIGH)
**Root Cause:** `Bun.serve()` fetch-Handler nutzte `app.fetch` direkt, ohne
`server.upgrade(req)` fuer `/ws` Pfad aufzurufen. WebSocket-Verbindungen
wurden nie upgegraded.
**Fix:** Expliziter `/ws` Pfad-Check mit `server.upgrade(req)` im fetch-Handler.

### BUG-3: UFW blockierte Dashboard-Port (MEDIUM)
**Root Cause:** VM-Firewall erlaubte nur SSH. Browser konnte Dashboard nicht
erreichen, SSH-Tunnel verursachte WebSocket-Probleme.
**Fix:** UFW-Regeln fuer alle Sentinel-Ports (8000-8083, 8222) von 10.0.0.0/24.

---

## Zusammenfassung

| Kategorie | Pass | Fail | Total |
|-----------|------|------|-------|
| Infrastructure & Services | 16 | 0 | 16 |
| Dashboard UI | 15 | 0 | 15 |
| Release Manifest | 3 | 0 | 3 |
| Projection Worker | 4 | 0 | 4 |
| Outbox Drain | 5 | 0 | 5 |
| **TOTAL** | **43** | **0** | **43** |

### Release-Gate: PASS
- 0 CRITICAL/HIGH offene Findings
- Alle Services aktiv und gesund
- Manifest 31/31 Artefakte verifiziert
- Dashboard funktional mit echten DB-Daten
- WebSocket verbunden, Lag = 0
- Outbox vollstaendig gedrained (0 pending)

### Known Limitations
- Agents-View leer (keine Agent-Simulation aktiv — kein Bug)
- Aktivitaet-View leer (keine Agent-Aktionen — kein Bug)
- Cockpit zeigt nur Top-200 Incidents (Performance-Limit)
- Nightrun consolidated=0 (erwartet ohne aktive Agents)

### Geaenderte Dateien (waehrend E2E gefixt)
- `dashboard/src/index.ts` — WebSocket-Upgrade + CPU-Monitor
- `dashboard/src/db.ts` — getRecentIncidentEvents LIMIT Parameter
- `dashboard/src/routes/cockpit.ts` — Cockpit LIMIT Parameter
- `crates/sentinel-limbo/src/event_store.rs` — causation_id Index
- `pkg/sentinel-go/eventstore/store.go` — causation_id Index
