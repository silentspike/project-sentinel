# Verifikations-Report — Issue #384: Time-Travel Debugging UI

**Issue:** #384 — Time-Travel Debugging UI: Snapshot-Navigation + Restore-Flow im Dashboard (TOGAF Cluster 11)
**Branch:** `feat/issue-384-timetravel-ui`
**Datum:** 2026-05-29
**Scope:** Reine Frontend-/Dashboard-Arbeit (`dashboard/`). Backend (WorldSnapshot,
Hot-Swap-Restore, Tiered Retention) existiert bereits und wurde nicht angefasst.

## Zusammenfassung der Aenderungen

| Datei | Aenderung |
|-------|-----------|
| `dashboard/src/types.ts` | Typen `SnapshotWorldState`, `SnapshotRoomOccupancy` |
| `dashboard/src/db.ts` | `getSnapshotWorldState()` — Welt-Zustand per Event-Replay bis `last_event_id` |
| `dashboard/src/routes/control.ts` | `GET /api/control/snapshot-state` |
| `dashboard/public/js/timetravel.js` | **NEU** — Zeitreise-View: Timeline, Welt-Zustand, Restore |
| `dashboard/public/index.html` | Nav-Button + `<section id="view-timetravel">` |
| `dashboard/public/js/app.js` | Wiring + Fix des kaputten `snapshot_restored`-Handlers (AC-4) |
| `dashboard/public/css/style.css` | Timeline-/Detail-Panel-Styles |
| `dashboard/src/__tests__/control.test.ts` | 3 Tests fuer `snapshot-state` |

## Statische Verifikation

```
$ bun run typecheck     → tsc --noEmit, EXIT 0 (keine Fehler)
$ bun test              → 78 pass, 0 fail, 673 expect() calls (11 Dateien)
```
Auf der Deploy-VM nach Deploy ebenfalls `bun run typecheck` → EXIT 0.

## Architektur-Entscheidung AC-2 (Welt-Zustand-Ableitung)

`SnapshotMeta` (das Snapshot-Listing) traegt **keine** Agent-/Raum-Counts, und der
Snapshot-Payload ist ein bincode-2-BLOB (in Bun nicht dekodierbar). Daher wird der
Welt-Zustand zum Snapshot-Zeitpunkt **deterministisch aus dem EventStore abgeleitet**
(den das Dashboard ohnehin read-only liest) — kein neuer Rust-Endpoint, kein Gateway,
kein LLM-Call (token-safe):

- **`active_agent_count`** = `agent_count` aus dem naechsten `tick_snapshot`-Event
  bei/vor `tick`. Authoritativ und deckungsgleich mit der Live-Agents-View.
- **`present_agent_count`** + **Raum-Belegung** = Replay der `agent_spawned` /
  `transit_completed` / `agent_despawned`-Events bis `last_event_id` (Agents im
  Gebaeude inkl. Off-Shift, mit ihrem zuletzt bekannten Raum).

---

## Acceptance Criteria — Evidence

### AC-1: UI listet Snapshots mit Tier-Badge, Zeitstempel, Tick, Groesse entlang einer Zeitachse

**Status: PASS**

API-Beleg (echte VM-Daten):
```
$ curl -s http://10.0.0.240:8000/api/control/snapshots
[{"id":"019e7409-...","tier":"hourly","tick":1476529,"sim_hour":21.745188,
  "payload_size_bytes":566324,"created_at_ms":1780063232607}, ... 59 Snapshots]
```
Playwright-Screenshot `issue384-ac1-timeline.png`: visuelle Zeitachse mit
Achsen-Labels (17.3.2026 → 29.5.2026), tier-codierten Markern (hourly gruen,
monthly orange), Legende, sowie Snapshot-Liste (59) mit Spalten Tier-Badge,
Zeitstempel, Tick, Sim Hour, Groesse.

### AC-2: Auswahl eines Snapshots zeigt den Welt-Zustand (Agent-/Raum-Counts) dieses Zeitpunkts

**Status: PASS**

API-Beleg (Snapshot tick 1476529):
```
$ curl -s "http://10.0.0.240:8000/api/control/snapshot-state?snapshot_id=019e7409-625d-7703-95eb-2bee0815a7c3"
{"snapshot_id":"019e7409-...","tier":"hourly","tick":1476529,"last_event_id":10735662,
 "active_agent_count":26,"present_agent_count":43,"room_count":12,
 "rooms":[{"room_id":"buero-dev-1","name":"Entwicklungsbüro 1","occupant_count":9}, ...]}
```
Konsistenz-Check: `active_agent_count` (26) == Live-`/api/agents` aktive Agents (26).
Summe der Raum-Belegung (9+6+6+4+3+3+3+2+2+2+2+1) == `present_agent_count` (43).

Unit-Test `control.test.ts` "derives agent and room counts at the snapshot boundary":
verifiziert Replay-Grenze (`last_event_id`), Despawn-Handling, Events hinter der
Grenze werden ignoriert, Raum-Namen-Aufloesung. PASS.

Playwright-Screenshots `issue384-ac2-worldstate.png` (26 aktive / 43 im Gebaeude /
12 belegte Raeume + Meta) und `issue384-ac2-rooms.png` (Pro-Raum-Belegung mit
Umlaut-korrekten Raum-Namen + Restore-Button).

### AC-3: Restore-Button loest Hot-Swap-Restore via bestehende API aus (mit Confirm-Dialog)

**Status: PASS**

Confirm-Dialog beim Klick verifiziert (Playwright: Klick → Dialog erscheint →
`dialog-dismiss` bricht ab, kein Restore). Anschliessend Restore ausgeloest;
Daemon-Journal (10.0.0.240) bestaetigt den vollen Hot-Swap-Pfad ueber die
bestehende Operator-API:
```
INFO sentinel_daemon::operator_api: Restore via Operator-API angefordert snapshot_id=019e7409-...
INFO sentinel_daemon::orchestrator: Hot-Swap Restore gestartet snapshot_id=019e7409-...
INFO sentinel_daemon::orchestrator: Pre-Restore Snapshot erstellt (Rollback-Punkt) snapshot_id=019e7442-...
INFO sentinel_daemon::orchestrator: Agent-Prozesse fuer Restore terminiert terminated=26
INFO sentinel_daemon::orchestrator: Projection Snapshot-Seeding abgeschlossen agents_seeded=26 rooms_seeded=11
INFO sentinel_daemon::orchestrator: Hot-Swap Restore abgeschlossen snapshot_id=019e7409-... tick=1476529 agents=26
```
Restore ist reversibel — der Daemon erstellt automatisch einen Pre-Restore-Rollback-Snapshot.

### AC-4: Nach Restore aktualisiert sich das Dashboard live (WebSocket Event)

**Status: PASS**

Latenter Bug gefixt: der bisherige `snapshot_restored`-Handler in `app.js` rief
undefinierte Funktionen `loadAgents()`/`loadRooms()` auf (ReferenceError) — ersetzt
durch `reloadAfterRestore()` (Fetch `/api/agents` + `/api/rooms`, Re-Render).

Runtime-Beleg via Spy-WebSocket auf `/ws` (Playwright `eval`): nach Restore
empfangene Frame-Sequenz:
```
["health_update","snapshot_restored","agent_update","room_update",
 "cockpit_update","chaos_update","activity_update"]
```
Der Server broadcastet `snapshot_restored`; `resetWatermarks()` erzwingt den
folgenden vollen `agent_update`/`room_update`-Broadcast. Playwright-Screenshot
`issue384-ac4-live-agents.png`: Agents-View zeigt ohne manuellen Reload den
wiederhergestellten Zustand (`autonomy:bio_emergency T1476609`, direkt nach
Restore-Tick 1476529).

### AC-5: Playwright-Verifikation: Snapshot waehlen, Restore ausloesen, Screenshot

**Status: PASS**

Vollstaendiger E2E-Durchlauf gegen `http://10.0.0.240:8000` mit playwright-cli:
Navigation Zeitreise-Tab → Snapshot-Marker waehlen → Welt-Zustand → Confirm-Dialog →
Restore → Live-Update. Screenshots: `issue384-ac1-timeline.png`,
`issue384-ac2-worldstate.png`, `issue384-ac2-rooms.png`,
`issue384-ac3-restore-feedback.png`, `issue384-ac4-live-agents.png`
(in `/home/jan/Pictures/Screenshots/`).

---

## Deploy + Health (Deploy-VM 10.0.0.240)

```
$ grep ExecStart /etc/systemd/system/sentinel-dashboard.service
  ExecStart=/usr/local/bin/bun run start   (WorkingDirectory=/opt/sentinel/dashboard, PORT=8000)
$ sudo systemctl restart sentinel-dashboard → active, "Dashboard running on http://localhost:8000"
$ systemctl is-active sentinel-dashboard sentinel-daemon sentinel-projection → active/active/active
$ journalctl -u sentinel-daemon (3 min) → 0 panics / FATAL
```
`cortex-gateway` blieb wie gefordert INACTIVE (Token-Schutz); #384 benoetigt keinen
Gateway und keine LLM-Calls.

## Nicht getestet / Hinweise

- Tier-Filter/Such-UI: nicht in Scope (#384 verlangt nur Navigation + Restore).
- `buero-design` ohne ROOM_METADATA-Eintrag faellt graceful auf die room_id zurueck
  (bestehende Config-Drift, nicht Teil dieses Issues).

## Confidence

**95 %** — Alle 5 ACs mit Command+Output bzw. Screenshot im laufenden System belegt
(Backend-API gegen echte Daten, Hot-Swap-Restore im Daemon-Journal, Live-Update via
WS-Frame). Restzweifel nur bzgl. langfristiger UI-Robustheit bei stark wachsender
Snapshot-Zahl (Timeline-Marker-Dichte).
