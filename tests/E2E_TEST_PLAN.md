# Project Sentinel - E2E Abnahme-Testplan

> **Zweck:** Vollstaendige End-to-End-Validierung aller implementierten Features gegen den Masterplan.
> **Tool:** Playwright (Browser-Tests) + curl/SSH (Service-Tests)
> **Target:** Deploy-VM `10.0.0.240` (Dashboard: Port 8000, Cortex: 8080/8081, Judge: 8082, Bridge: 8083, NATS: 4222)
> **Erstellt:** 2026-02-21

---

## Legende

| Kuerzel | Bedeutung |
|---------|-----------|
| **PW** | Playwright Browser-Test |
| **HTTP** | curl/fetch gegen API |
| **SSH** | Pruefung auf Deploy-VM via SSH |
| **CLI** | Lokaler CLI-Aufruf |
| **NATS** | NATS CLI (`nats stream/consumer info`) |

| Prioritaet | Bedeutung |
|-------------|-----------|
| **P0** | Release-Blocker — muss bestehen |
| **P1** | Wichtig — sollte bestehen |
| **P2** | Nice-to-have — kann nachgereicht werden |

---

## T1: Infrastruktur Health-Checks (Pre-Flight)

Bevor IRGENDEIN anderer Test laeuft, muessen alle Services erreichbar sein.

### T1.1 — Dashboard erreichbar [P0] [HTTP]
- **Aktion:** `GET http://10.0.0.240:8000/`
- **Erwartung:** HTTP 200, Response enthaelt `<title>` mit "Project Sentinel"
- **Fail-Kriterium:** Kein Response oder Status != 200

### T1.2 — Cortex Gateway Proxy Health [P0] [HTTP]
- **Aktion:** `GET http://10.0.0.240:8080/health`
- **Erwartung:** HTTP 200, JSON `{"status":"ok","version":"0.1.0"}`
- **Fail-Kriterium:** Status != 200 oder fehlende Felder

### T1.3 — Cortex Gateway Proxy Ready [P0] [HTTP]
- **Aktion:** `GET http://10.0.0.240:8080/ready`
- **Erwartung:** HTTP 200, JSON `{"ready":true}`
- **Fail-Kriterium:** Status 503 oder `ready: false`

### T1.4 — Cortex Gateway Control Plane [P0] [HTTP]
- **Aktion:** `GET http://10.0.0.240:8081/control/config`
- **Erwartung:** HTTP 200, JSON mit `primary_provider`, `temperature`, `max_tokens`
- **Fail-Kriterium:** Status != 200

### T1.5 — Sentinel Judge Health [P0] [HTTP]
- **Aktion:** `GET http://10.0.0.240:8082/health`
- **Erwartung:** HTTP 200, JSON `{"status":"ok","service":"sentinel-judge"}`
- **Fail-Kriterium:** Status != 200

### T1.6 — Sentinel Judge Ready [P0] [HTTP]
- **Aktion:** `GET http://10.0.0.240:8082/ready`
- **Erwartung:** HTTP 200, JSON `{"ready":true}`
- **Fail-Kriterium:** Status 503 (NATS nicht connected)

### T1.7 — NATS Bridge Health [P0] [HTTP]
- **Aktion:** `GET http://10.0.0.240:8083/health`
- **Erwartung:** HTTP 200, JSON `{"status":"ok","service":"sentinel-nats-bridge"}`
- **Fail-Kriterium:** Status != 200

### T1.8 — NATS Server erreichbar [P0] [SSH]
- **Aktion:** `ssh ubuntu@10.0.0.240 "nc -zv 127.0.0.1 4222"`
- **Erwartung:** Connection succeeded
- **Fail-Kriterium:** Connection refused

### T1.9 — Sentinel Daemon laeuft [P0] [SSH]
- **Aktion:** `ssh ubuntu@10.0.0.240 "systemctl is-active sentinel-daemon.service"`
- **Erwartung:** Output "active"
- **Fail-Kriterium:** "inactive" oder "failed"

### T1.10 — Alle systemd Services aktiv [P0] [SSH]
- **Aktion:** `ssh ubuntu@10.0.0.240 "systemctl is-active sentinel-daemon sentinel-cortex sentinel-dashboard sentinel-projection sentinel-judge sentinel-nats-bridge nats-server"`
- **Erwartung:** Alle 7 zeigen "active"
- **Fail-Kriterium:** Mindestens einer nicht "active"

### T1.11 — Cortex Control Plane Health [P0] [HTTP]
- **Aktion:** `GET http://10.0.0.240:8081/health`
- **Erwartung:** HTTP 200, JSON `{"status":"ok"}`
- **Fail-Kriterium:** Status != 200

### T1.12 — NATS Bridge Ready [P0] [HTTP]
- **Aktion:** `GET http://10.0.0.240:8083/ready`
- **Erwartung:** HTTP 200, JSON `{"status":"ok",...}` (NATS connected)
- **Fail-Kriterium:** Status 503 (NATS disconnected)

### T1.13 — Dashboard Health [P0] [HTTP]
- **Aktion:** `GET http://10.0.0.240:8000/api/health`
- **Erwartung:** HTTP 200, JSON `{"status":"ok","uptime":N,"projection_lag":N}`
- **Fail-Kriterium:** Status != 200 oder fehlende Felder

### T1.14 — Sentinel Projection Worker laeuft [P0] [SSH]
- **Aktion:** `ssh ubuntu@10.0.0.240 "systemctl is-active sentinel-projection"`
- **Erwartung:** Output "active"
- **Fail-Kriterium:** "inactive" oder "failed"

### T1.15 — NATS Monitoring Port [P1] [HTTP]
- **Aktion:** `GET http://10.0.0.240:8222/varz`
- **Erwartung:** HTTP 200, JSON mit NATS Server-Info
- **Fail-Kriterium:** Port nicht erreichbar

---

## T2: Dashboard — Navigation & Layout

### T2.1 — Seite laed komplett [P0] [PW]
- **Aktion:** Playwright `open http://10.0.0.240:8000`, warte auf DOM ready
- **Erwartung:** Titel enthaelt "Project Sentinel", keine Console-Errors
- **Pruefung:** `document.title`, Console-Log auf Errors pruefen

### T2.2 — 7 Navigationsbuttons sichtbar [P0] [PW]
- **Aktion:** Alle `.nav-btn` Elemente zaehlen
- **Erwartung:** Genau 7 Buttons: "Agents", "Bueroplan", "Aktivitaet", "Chaos", "Chat", "Metriken", "Cockpit"
- **Pruefung:** `querySelectorAll('.nav-btn').length === 7`

### T2.3 — Navigation: Agents-View [P0] [PW]
- **Aktion:** Klick auf Button `[data-view="agents"]`
- **Erwartung:** `#view-agents` sichtbar, alle anderen Views hidden
- **Pruefung:** `#view-agents` hat `display != none`, `#view-floorplan` etc. hidden

### T2.4 — Navigation: Bueroplan-View [P0] [PW]
- **Aktion:** Klick auf Button `[data-view="floorplan"]`
- **Erwartung:** `#view-floorplan` sichtbar, Rest hidden
- **Pruefung:** Analog T2.3

### T2.5 — Navigation: Aktivitaet-View [P0] [PW]
- **Aktion:** Klick auf Button `[data-view="activity"]`
- **Erwartung:** `#view-activity` sichtbar, Rest hidden
- **Pruefung:** Analog T2.3

### T2.6 — Navigation: Metriken-View [P0] [PW]
- **Aktion:** Klick auf Button `[data-view="metrics"]`
- **Erwartung:** `#view-metrics` sichtbar, Rest hidden
- **Pruefung:** Analog T2.3

### T2.7 — Navigation: Cockpit-View [P0] [PW]
- **Aktion:** Klick auf Button `[data-view="cockpit"]`
- **Erwartung:** `#view-cockpit` sichtbar, Rest hidden
- **Pruefung:** Analog T2.3

### T2.7a — Navigation: Chaos-View [P0] [PW]
- **Aktion:** Klick auf Button `[data-view="chaos"]`
- **Erwartung:** `#view-chaos` sichtbar, Rest hidden
- **Pruefung:** Analog T2.3

### T2.7b — Navigation: Chat-View [P0] [PW]
- **Aktion:** Klick auf Button `[data-view="chat"]`
- **Erwartung:** `#view-chat` sichtbar, Rest hidden
- **Pruefung:** Analog T2.3

### T2.8 — Nur ein View gleichzeitig sichtbar [P0] [PW]
- **Aktion:** Nacheinander alle 7 Buttons klicken, nach jedem Klick pruefen
- **Erwartung:** Zu jedem Zeitpunkt genau 1 View sichtbar
- **Pruefung:** `querySelectorAll('.view.active').length === 1`

### T2.9 — Projection Lag Anzeige [P0] [PW]
- **Aktion:** Element `#projection-lag` lesen
- **Erwartung:** Numerischer Wert oder "0", sichtbar im Header
- **Pruefung:** `#projection-lag.textContent` ist Zahl

### T2.10 — Projection Lag Farbcodierung [P1] [PW]
- **Aktion:** CSS-Klasse von `#projection-lag` pruefen
- **Erwartung:** `lag-ok` (gruen, <10), `lag-medium` (gelb, 10-100), oder `lag-high` (rot, >100)
- **Pruefung:** Element hat eine der drei Klassen

### T2.11 — Connection Status Anzeige [P1] [PW]
- **Aktion:** Element `#connection-status` pruefen
- **Erwartung:** Zeigt WebSocket-Verbindungsstatus
- **Pruefung:** Element existiert und hat Text-Inhalt

### T2.12 — Kein innerHTML im DOM [P0] [PW]
- **Aktion:** Alle JS-Dateien auf `innerHTML` durchsuchen
- **Erwartung:** 0 Treffer — nur `textContent` erlaubt (Security)
- **Pruefung:** `grep -r "innerHTML" dashboard/public/js/` ergibt 0 Treffer

---

## T3: Dashboard — Agents View

### T3.1 — Agent-Cards werden gerendert [P0] [PW]
- **Aktion:** Agents-View oeffnen, `.agent-card` Elemente zaehlen
- **Erwartung:** Mindestens 1 Card (aktive Schicht hat 12-15 Agents)
- **Pruefung:** `querySelectorAll('.agent-card').length >= 1`

### T3.2 — Agent-Card zeigt Name [P0] [PW]
- **Aktion:** Erste `.agent-card` inspizieren
- **Erwartung:** `h3` Element mit Agent-Name (nicht leer)
- **Pruefung:** `.agent-card h3.textContent.length > 0`

### T3.3 — Agent-Card zeigt Rolle [P0] [PW]
- **Aktion:** Erste `.agent-card .role` inspizieren
- **Erwartung:** Rollentext vorhanden (z.B. "Senior Developer", "Designer")
- **Pruefung:** `.agent-card .role.textContent.length > 0`

### T3.4 — Agent-Card zeigt Status-Badge [P0] [PW]
- **Aktion:** `.agent-card .status-badge` pruefen
- **Erwartung:** Klasse `status-active`, `status-despawned` oder `status-paused`
- **Pruefung:** Element hat mindestens eine Status-Klasse

### T3.5 — Agent-Card zeigt aktuellen Raum [P0] [PW]
- **Aktion:** `.agent-card .room` lesen
- **Erwartung:** Raumname (z.B. "Buero Dev 1") oder Transit-Info
- **Pruefung:** `.room.textContent.length > 0`

### T3.6 — Transit-Status korrekt angezeigt [P1] [PW]
- **Aktion:** Agents im Transit finden (`.transit` Klasse)
- **Erwartung:** Text "Unterwegs → [Zielraum]"
- **Pruefung:** Text enthaelt Pfeil und Zielraum-Name

### T3.7 — Agent-Detail async geladen [P1] [PW]
- **Aktion:** Agent-Card oeffnen, auf Detaildaten warten (max 2s)
- **Erwartung:** Shift-Set und Last-Event-ID nachgeladen
- **Pruefung:** `.agent-meta` Element hat Inhalt

### T3.8 — Agent-Card Grid-Layout [P1] [PW]
- **Aktion:** CSS Grid pruefen
- **Erwartung:** Responsive Grid (`minmax(300px, 1fr)`)
- **Pruefung:** `getComputedStyle` zeigt Grid-Layout

### T3.9 — Alle aktiven Agents haben Cards [P0] [PW]
- **Aktion:** API `/api/agents` abfragen, Card-Count vergleichen
- **Erwartung:** Anzahl Cards == Anzahl aktive Agents aus API
- **Pruefung:** DOM-Count === API-Count

---

## T4: Dashboard — Bueroplan (Floorplan) View

### T4.1 — Etagen-Gruppierung vorhanden [P0] [PW]
- **Aktion:** Floorplan-View oeffnen, Etagen-Header zaehlen
- **Erwartung:** 3 Gruppen: OG (1. OG), EG (Erdgeschoss), Treppenhaus
- **Pruefung:** 3 Floor-Group Container

### T4.2 — Etagen absteigend sortiert [P1] [PW]
- **Aktion:** Reihenfolge der Floor-Groups pruefen
- **Erwartung:** OG (floor=1) → EG (floor=0) → Treppenhaus (floor=-1)
- **Pruefung:** DOM-Reihenfolge der Gruppen

### T4.3 — 17 Raum-Cards total [P0] [PW]
- **Aktion:** Alle `.room-card` zaehlen
- **Erwartung:** Exakt 17 Raeume
- **Pruefung:** `querySelectorAll('.room-card').length === 17`

### T4.4 — Raum-Card zeigt Name [P0] [PW]
- **Aktion:** `.room-card h4` lesen
- **Erwartung:** Deutscher Raumname (z.B. "Empfang", "Buero Dev 1")
- **Pruefung:** `h4.textContent.length > 0`

### T4.5 — Raum-Card zeigt Typ-Badge [P1] [PW]
- **Aktion:** `.room-card .room-type` pruefen
- **Erwartung:** Typ vorhanden (common, transit, break, office, meeting, bathroom)
- **Pruefung:** `.room-type.textContent` ist einer der 6 Typen

### T4.6 — Belegungszahl angezeigt [P0] [PW]
- **Aktion:** `.room-card .room-occupancy` lesen
- **Erwartung:** Numerischer Wert (>= 0)
- **Pruefung:** Ist Zahl, Klasse `occupied` wenn > 0

### T4.7 — Transit-Indikator [P1] [PW]
- **Aktion:** `.room-card .transit-indicator` pruefen
- **Erwartung:** Zeigt Anzahl Agents im Transit zum Raum
- **Pruefung:** Element existiert, numerischer Wert

### T4.8 — Chaos-Badge bei aktivem Chaos [P1] [PW]
- **Aktion:** Raeume mit `active_chaos != null` finden
- **Erwartung:** `.chaos-badge` sichtbar mit Chaos-Typ
- **Pruefung:** Badge-Text enthaelt Chaos-Typ

### T4.9 — Raumdaten stimmen mit API ueberein [P0] [PW]
- **Aktion:** `/api/rooms` abfragen, mit DOM vergleichen
- **Erwartung:** Jeder API-Raum hat ein DOM-Element mit korrekten Daten
- **Pruefung:** Name, Typ, Belegung matchen

### T4.10 — EG-Raeume korrekt [P1] [PW]
- **Aktion:** EG-Gruppe pruefen
- **Erwartung:** 7 Raeume: empfang, flur-eg, kueche, buero-dev-1, buero-dev-2, meetingraum-01, toilette-eg
- **Pruefung:** IDs der Room-Cards in EG-Gruppe

### T4.11 — OG-Raeume korrekt [P1] [PW]
- **Aktion:** OG-Gruppe pruefen
- **Erwartung:** 7 Raeume: flur-og, buero-design-1, buero-design-2, buero-ceo, meetingraum-02, meetingraum-03, toilette-og
- **Pruefung:** IDs der Room-Cards in OG-Gruppe

### T4.12 — Treppenhaus korrekt [P1] [PW]
- **Aktion:** Treppenhaus-Gruppe pruefen
- **Erwartung:** 1 Raum: treppenhaus
- **Pruefung:** Genau 1 Room-Card

### T4.13 — Agent-Positionen in Room-Cards [P0] [PW]
- **Aktion:** `.room-agents` Container in Room-Cards pruefen
- **Erwartung:** Belegte Raeume zeigen `.room-agent-tag` Elemente mit Agent-Namen
- **Pruefung:** Raum mit `occupant_count > 0` hat `.room-agent-tag` Kinder

### T4.14 — Kapazitaet angezeigt [P1] [PW]
- **Aktion:** `.room-occupancy` Text lesen
- **Erwartung:** Format "X/Y Personen" (Belegung/Kapazitaet)
- **Pruefung:** Regex `\d+/\d+ Personen`

### T4.15 — API occupants Feld vorhanden [P0] [HTTP]
- **Aktion:** `GET /api/rooms`, Response-Schema pruefen
- **Erwartung:** Jeder Raum hat `occupants` Array (kann leer sein)
- **Pruefung:** `Array.isArray(room.occupants)`

---

## T5: Dashboard — Aktivitaet (Activity) View

### T5.1 — Activity-Liste gerendert [P0] [PW]
- **Aktion:** Activity-View oeffnen
- **Erwartung:** Liste mit Activity-Items ODER Empty-State
- **Pruefung:** `.activity-item` Elemente oder Empty-State-Text

### T5.2 — Activity-Item zeigt Agent-Name [P0] [PW]
- **Aktion:** `.activity-item .activity-agent` lesen
- **Erwartung:** Agent-Name (nicht leer)
- **Pruefung:** `span.activity-agent.textContent.length > 0`

### T5.3 — Activity-Item zeigt Detail [P0] [PW]
- **Aktion:** `.activity-item .activity-detail` lesen
- **Erwartung:** Transit-Info ("Unterwegs nach...") oder Aktionstext
- **Pruefung:** `span.activity-detail.textContent.length > 0`

### T5.4 — Maximal 50 Items [P1] [PW]
- **Aktion:** Alle `.activity-item` zaehlen
- **Erwartung:** <= 50
- **Pruefung:** `querySelectorAll('.activity-item').length <= 50`

### T5.5 — Sortierung nach Tick absteigend [P1] [PW]
- **Aktion:** Tick-Werte der Items extrahieren
- **Erwartung:** Absteigend sortiert (neueste zuerst)
- **Pruefung:** items[i].tick >= items[i+1].tick

### T5.6 — Empty State korrekt [P1] [PW]
- **Aktion:** Bei leerer DB pruefen
- **Erwartung:** Text "Keine Aktivitaeten vorhanden"
- **Pruefung:** Fallback-Text sichtbar

---

## T5a: Dashboard — Chaos Event Feed View

### T5a.1 — Chaos-View navigierbar [P0] [PW]
- **Aktion:** Klick auf Button `[data-view="chaos"]`
- **Erwartung:** `#view-chaos` sichtbar, Header "Chaos Event Feed" vorhanden
- **Pruefung:** `.chaos-header h2.textContent` enthaelt "Chaos"

### T5a.2 — Chaos-Events geladen [P0] [PW]
- **Aktion:** `.chaos-item` Elemente zaehlen
- **Erwartung:** Mindestens 1 Event (221k chaos_triggered in events.db)
- **Pruefung:** `querySelectorAll('.chaos-item').length > 0`

### T5a.3 — Chaos-Item Struktur korrekt [P0] [PW]
- **Aktion:** Erstes `.chaos-item` untersuchen
- **Erwartung:** Enthaelt `.chaos-type-badge`, `.chaos-description`, `.chaos-meta`
- **Pruefung:** Alle Sub-Elemente vorhanden

### T5a.4 — Chaos-Count Badge [P1] [PW]
- **Aktion:** `.chaos-count` Element lesen
- **Erwartung:** Zeigt "N Events" mit N > 0
- **Pruefung:** Regex `\d+ Events`

### T5a.5 — Chaos-API Contract [P0] [HTTP]
- **Aktion:** `GET /api/chaos?limit=10`
- **Erwartung:** JSON Array, jedes Element hat `id`, `event_id`, `chaos_type`, `room_id`, `description`, `tick`, `timestamp_ms`
- **Pruefung:** Schema-Validierung

### T5a.6 — Chaos-API Room Filter [P1] [HTTP]
- **Aktion:** `GET /api/chaos/buero-dev-1`
- **Erwartung:** JSON Array, alle Eintraege haben `room_id === "buero-dev-1"` oder sind leer
- **Pruefung:** Filter funktioniert

### T5a.7 — Chaos WebSocket Update [P1] [PW]
- **Aktion:** WebSocket auf `chaos_update` Message warten
- **Erwartung:** Message mit `type: "chaos_update"` empfangen
- **Pruefung:** WS-Event Typ pruefen

---

## T5b: Dashboard — Chat View

### T5b.1 — Chat-View navigierbar [P0] [PW]
- **Aktion:** Klick auf Button `[data-view="chat"]`
- **Erwartung:** `#view-chat` sichtbar
- **Pruefung:** `#view-chat.classList.contains('active')`

### T5b.2 — Chat-Filter-Bar vorhanden [P0] [PW]
- **Aktion:** `.chat-filter-bar` Element suchen
- **Erwartung:** Filter-Bar mit "Alle" Button sichtbar
- **Pruefung:** `.chat-filter-btn` mit Text "Alle" existiert

### T5b.3 — Chat-API Contract [P0] [HTTP]
- **Aktion:** `GET /api/chat?limit=10`
- **Erwartung:** JSON Array (kann leer sein wenn keine Simulation laeuft)
- **Pruefung:** Valides JSON Array

### T5b.4 — Chat Room-Filter API [P1] [HTTP]
- **Aktion:** `GET /api/chat/buero-dev-1`
- **Erwartung:** JSON Array gefiltert nach Raum
- **Pruefung:** Valides JSON Array

### T5b.5 — Empty State bei fehlenden Nachrichten [P0] [PW]
- **Aktion:** Chat-View oeffnen (ohne laufende Simulation)
- **Erwartung:** "Keine Nachrichten vorhanden" Text sichtbar
- **Pruefung:** `.chat-empty` Element vorhanden

---

## T6: Dashboard — Metriken View

### T6.1 — 8 Metrik-Cards sichtbar [P0] [PW]
- **Aktion:** Metriken-View oeffnen, `.metric-card` zaehlen
- **Erwartung:** Exakt 8 Cards (inkl. Events Gesamt und Events/Min)
- **Pruefung:** `querySelectorAll('.metric-card').length === 8`

### T6.2 — Aktive Agents Metrik [P0] [PW]
- **Aktion:** Card "Aktive Agents" finden
- **Erwartung:** Numerischer Wert > 0 (laufende Schicht)
- **Pruefung:** Wert ist Zahl, > 0

### T6.3 — Aktionen Metrik [P0] [PW]
- **Aktion:** Card "Aktionen" finden
- **Erwartung:** Numerischer Wert (total_actions)
- **Pruefung:** Wert ist Zahl

### T6.4 — Transits Metrik [P0] [PW]
- **Aktion:** Card "Transits" finden
- **Erwartung:** Numerischer Wert (total_transits)
- **Pruefung:** Wert ist Zahl

### T6.5 — Chaos Events Metrik [P0] [PW]
- **Aktion:** Card "Chaos Events" finden
- **Erwartung:** Numerischer Wert (chaos_events)
- **Pruefung:** Wert ist Zahl

### T6.6 — Schichtwechsel Metrik [P0] [PW]
- **Aktion:** Card "Schichtwechsel" finden
- **Erwartung:** Numerischer Wert (shift_changes)
- **Pruefung:** Wert ist Zahl

### T6.7 — Uptime korrekt formatiert [P0] [PW]
- **Aktion:** Card "Uptime" lesen
- **Erwartung:** Format "Xh Ym" (z.B. "21h 10m")
- **Pruefung:** Regex `\d+h \d+m`

### T6.8 — Metriken stimmen mit API ueberein [P0] [PW]
- **Aktion:** `/api/metrics` abfragen, mit DOM-Werten vergleichen
- **Erwartung:** Alle 8 Werte identisch
- **Pruefung:** API-Response === DOM-Values

### T6.9 — Events Gesamt Metrik [P1] [PW]
- **Aktion:** Card "Events Gesamt" finden
- **Erwartung:** Numerischer Wert > 0 (z.B. "221.9k")
- **Pruefung:** Wert ist formatierte Zahl

### T6.10 — Events/Min Metrik [P1] [PW]
- **Aktion:** Card "Events/Min" finden
- **Erwartung:** Numerischer Wert >= 0
- **Pruefung:** Wert ist Zahl

---

## T7: Dashboard — Cockpit View (Issue #108)

### T7.1 — SLO-Leiste sichtbar [P0] [PW]
- **Aktion:** Cockpit-View oeffnen, `.cockpit-slo-bar` pruefen
- **Erwartung:** SLO-Leiste mit 4 Metriken sichtbar
- **Pruefung:** Element existiert, 4 SLO-Items drin

### T7.2 — SLO: Projection Lag [P0] [PW]
- **Aktion:** SLO "Projection Lag" finden
- **Erwartung:** Aktueller Wert + Threshold (100) angezeigt, oder "OK"
- **Pruefung:** Text enthaelt Zahl oder "OK"

### T7.3 — SLO: Nightrun Failure-Rate [P0] [PW]
- **Aktion:** SLO "Nightrun Failure-Rate" finden
- **Erwartung:** Aktueller Wert + Threshold (10%) angezeigt, oder "OK"
- **Pruefung:** Analog T7.2

### T7.4 — SLO: Chaos-Frequenz [P0] [PW]
- **Aktion:** SLO "Chaos-Frequenz" finden
- **Erwartung:** Aktueller Wert + Threshold (3/h) angezeigt, oder "OK"
- **Pruefung:** Analog T7.2

### T7.5 — SLO: Despawn-Rate [P0] [PW]
- **Aktion:** SLO "Despawn-Rate" finden
- **Erwartung:** Aktueller Wert + Threshold (2/h) angezeigt, oder "OK"
- **Pruefung:** Analog T7.2

### T7.6 — Summary-Zeile [P0] [PW]
- **Aktion:** Summary-Text lesen
- **Erwartung:** Format "X aktiv / Y abgeschlossen (24h)"
- **Pruefung:** Regex `\d+ aktiv / \d+ abgeschlossen`

### T7.7 — Aktive Incidents Sektion [P0] [PW]
- **Aktion:** Aktive Incidents Bereich pruefen
- **Erwartung:** Sektion existiert (auch wenn leer)
- **Pruefung:** Container-Element vorhanden

### T7.8 — Resolved/Failed Sektion klappbar [P1] [PW]
- **Aktion:** Header der Resolved-Sektion klicken
- **Erwartung:** Sektion klappt auf/zu (toggle)
- **Pruefung:** Sichtbarkeit aendert sich nach Klick

### T7.9 — Incident Severity-Badge [P0] [PW]
- **Aktion:** Erstes Incident-Item inspizieren
- **Erwartung:** Severity-Badge mit CRIT, HIGH, MED, oder LOW
- **Pruefung:** Badge-Text ist einer der 4 Werte

### T7.10 — Incident Status-Badge [P0] [PW]
- **Aktion:** Incident `.cockpit-status-*` Klasse pruefen
- **Erwartung:** Einer von: `cockpit-status-active`, `-pending`, `-resolved`, `-failed`
- **Pruefung:** Element hat eine der 4 Klassen

### T7.11 — Incident Status-Text deutsch [P0] [PW]
- **Aktion:** Status-Badge Text lesen
- **Erwartung:** "Aktiv", "Ausstehend", "Geloest", oder "Fehlgeschlagen"
- **Pruefung:** Text ist einer der 4 deutschen Begriffe

### T7.12 — Incident Meta-Informationen [P0] [PW]
- **Aktion:** Incident Meta-Zeile lesen
- **Erwartung:** Tick-Nummer + Timestamp (de-DE Locale) + ggf. Agent-ID + Room-ID
- **Pruefung:** Tick ist Zahl, Timestamp-Format korrekt

### T7.13 — Incident Actions-Liste [P1] [PW]
- **Aktion:** Incident mit Actions finden, Actions-Liste pruefen
- **Erwartung:** Korrelierte Events als Action-Items angezeigt
- **Pruefung:** Mindestens 1 Action-Item mit event_type und summary

### T7.14 — Incident Outcome [P1] [PW]
- **Aktion:** Incident-Outcome lesen
- **Erwartung:** Outcome-Text oder "ausstehend" bei pending
- **Pruefung:** Outcome-Element hat Inhalt

### T7.15 — Nightrun-Incidents ohne Failures gefiltert [P1] [PW]
- **Aktion:** API `/api/cockpit` Response pruefen
- **Erwartung:** `nightrun_completed` Events mit 0 Failures werden NICHT als Incident gezeigt
- **Pruefung:** Kein Incident mit type=nightrun und 0 failures

### T7.16 — Cockpit-Daten stimmen mit API ueberein [P0] [PW]
- **Aktion:** `/api/cockpit` abfragen, DOM vergleichen
- **Erwartung:** Incident-Count, SLO-Werte, Summary matchen
- **Pruefung:** API === DOM

### T7.17 — Cockpit hours-Parameter [P1] [HTTP]
- **Aktion:** `/api/cockpit?hours=1` und `/api/cockpit?hours=168` abfragen
- **Erwartung:** Unterschiedliche Ergebnismenge, beide valide
- **Pruefung:** JSON-Schema korrekt, hours=1 <= hours=168

---

## T8: Dashboard — API Contract Tests

### T8.1 — GET /api/agents Response-Schema [P0] [HTTP]
- **Aktion:** `GET /api/agents`
- **Erwartung:** Array von AgentListItem mit Feldern: id, name, role, status, current_room, room_name, in_transit, transit_target, last_action, last_action_tick
- **Pruefung:** Alle Pflichtfelder vorhanden, Typen korrekt

### T8.2 — GET /api/agents/:id/state Detail [P0] [HTTP]
- **Aktion:** `GET /api/agents/1/state` (oder bekannte ID)
- **Erwartung:** AgentDetail mit zusaetzlich shift_set, last_event_id
- **Pruefung:** Erweiterte Felder vorhanden

### T8.3 — GET /api/agents/:id/state Slug-Lookup [P1] [HTTP]
- **Aktion:** `GET /api/agents/thomas-mueller/state` (Name-Slug)
- **Erwartung:** Gleicher Agent wie per ID
- **Pruefung:** Selbe Daten wie bei ID-Abfrage

### T8.4 — GET /api/agents/999/state 404 [P0] [HTTP]
- **Aktion:** `GET /api/agents/999/state`
- **Erwartung:** HTTP 404
- **Pruefung:** Status === 404

### T8.5 — GET /api/rooms Response-Schema [P0] [HTTP]
- **Aktion:** `GET /api/rooms`
- **Erwartung:** Array von RoomResponse mit: id, name, floor, capacity, room_type, occupant_count, transit_count, active_chaos, last_event_tick
- **Pruefung:** 15 Elemente, alle Felder vorhanden

### T8.6 — GET /api/rooms/:id Detail [P0] [HTTP]
- **Aktion:** `GET /api/rooms/buero-dev-1`
- **Erwartung:** Einzelner RoomResponse
- **Pruefung:** id === "buero-dev-1"

### T8.7 — GET /api/rooms/invalid 404 [P0] [HTTP]
- **Aktion:** `GET /api/rooms/nonexistent`
- **Erwartung:** HTTP 404
- **Pruefung:** Status === 404

### T8.8 — GET /api/metrics Response-Schema [P0] [HTTP]
- **Aktion:** `GET /api/metrics`
- **Erwartung:** MetricsResponse mit: active_agents, total_actions, total_transits, chaos_events, tick_count, shift_changes, nightrun_events, bucket_start, uptime
- **Pruefung:** Alle Felder numerisch

### T8.9 — GET /api/health Response-Schema [P0] [HTTP]
- **Aktion:** `GET /api/health`
- **Erwartung:** HealthResponse mit: status, uptime, projection_lag
- **Pruefung:** status === "ok", uptime >= 0, projection_lag >= 0

### T8.10 — GET /api/cockpit Response-Schema [P0] [HTTP]
- **Aktion:** `GET /api/cockpit`
- **Erwartung:** CockpitResponse mit: incidents (Array), slo_violations (Array), total_active, total_resolved_24h
- **Pruefung:** Alle Felder vorhanden, Typen korrekt

### T8.11 — GET /api/cockpit/incident/:id Detail [P0] [HTTP]
- **Aktion:** Zuerst `/api/cockpit` holen, erste Incident-ID nehmen, dann `/api/cockpit/incident/:id`
- **Erwartung:** Einzelnes CockpitIncident mit actions[] und outcome
- **Pruefung:** Alle Felder vorhanden

### T8.12 — GET /api/cockpit/incident/invalid 404 [P0] [HTTP]
- **Aktion:** `GET /api/cockpit/incident/nonexistent-id`
- **Erwartung:** HTTP 404
- **Pruefung:** Status === 404

### T8.13 — Content-Type Headers [P0] [HTTP]
- **Aktion:** Response-Header aller API-Endpoints pruefen
- **Erwartung:** `Content-Type: application/json` (nicht text/html)
- **Pruefung:** Header korrekt

### T8.14 — Static Files werden geliefert [P0] [HTTP]
- **Aktion:** `GET /public/css/style.css`, `GET /public/js/app.js`
- **Erwartung:** HTTP 200, korrekte Content-Types
- **Pruefung:** CSS = `text/css`, JS = `application/javascript`

---

## T9: Dashboard — WebSocket

### T9.1 — WebSocket Verbindung [P0] [PW]
- **Aktion:** Playwright WebSocket an `ws://10.0.0.240:8000/ws` verbinden
- **Erwartung:** Verbindung wird akzeptiert
- **Pruefung:** `onopen` Event empfangen

### T9.2 — agent_update Nachricht [P0] [PW]
- **Aktion:** WebSocket oeffnen, bis zu 10s auf Nachrichten warten
- **Erwartung:** Nachricht mit `type: "agent_update"` und `agents` Array
- **Pruefung:** JSON parsen, `type === "agent_update"`, `agents` ist Array

### T9.3 — room_update Nachricht [P0] [PW]
- **Aktion:** WebSocket oeffnen, bis zu 10s warten
- **Erwartung:** Nachricht mit `type: "room_update"` und `rooms` Array
- **Pruefung:** Analog T9.2

### T9.4 — health_update Nachricht [P0] [PW]
- **Aktion:** WebSocket oeffnen, bis zu 10s warten (health alle 5s)
- **Erwartung:** Nachricht mit `type: "health_update"`, `lag` und `uptime`
- **Pruefung:** `type === "health_update"`, `lag` ist Zahl

### T9.5 — cockpit_update Nachricht [P1] [PW]
- **Aktion:** WebSocket oeffnen, warten bis Incident-Event eintrifft
- **Erwartung:** Nachricht mit `type: "cockpit_update"`
- **Pruefung:** Typ korrekt (triggert Client-Refresh)

### T9.6 — WebSocket Reconnect [P1] [PW]
- **Aktion:** WebSocket verbinden, serverseitig trennen, 5s warten
- **Erwartung:** Client reconnected automatisch (3s Delay)
- **Pruefung:** Neue Verbindung nach Disconnect

### T9.7 — Lag-Anzeige aktualisiert per WebSocket [P0] [PW]
- **Aktion:** Dashboard oeffnen, 10s warten, `#projection-lag` mehrfach lesen
- **Erwartung:** Wert aendert sich (oder bleibt stabil bei 0)
- **Pruefung:** DOM-Update durch health_update Message

---

## T10: Dashboard — Styling & Responsive

### T10.1 — Dark Theme aktiv [P1] [PW]
- **Aktion:** Background-Color von `body` pruefen
- **Erwartung:** Dunkler Hintergrund (#1a1b2e oder aehnlich)
- **Pruefung:** `getComputedStyle(document.body).backgroundColor`

### T10.2 — Navigation Button Hover-State [P2] [PW]
- **Aktion:** Mouse-Hover ueber Nav-Button
- **Erwartung:** Visueller Hover-Effekt (Farbwechsel)
- **Pruefung:** CSS-Klasse oder Style aendert sich

### T10.3 — Aktiver Nav-Button hervorgehoben [P1] [PW]
- **Aktion:** Button klicken, CSS-Klasse pruefen
- **Erwartung:** Aktiver Button hat `.active` Klasse oder aehnlich
- **Pruefung:** `classList.contains('active')`

### T10.4 — Severity-Farben korrekt [P0] [PW]
- **Aktion:** Incident-Badges im Cockpit inspizieren
- **Erwartung:** CRIT=rot, HIGH=orange, MED=gelb, LOW=blau/grau
- **Pruefung:** `getComputedStyle` Farben der Badge-Elemente

---

## T11: Cortex Gateway (Issue #59, #58, #99)

### T11.1 — Health Endpoint [P0] [HTTP]
- **Aktion:** `GET http://10.0.0.240:8080/health`
- **Erwartung:** `{"status":"ok","version":"0.1.0"}`
- **Pruefung:** Status 200, JSON korrekt

### T11.2 — Ready Endpoint [P0] [HTTP]
- **Aktion:** `GET http://10.0.0.240:8080/ready`
- **Erwartung:** `{"ready":true}`
- **Pruefung:** Status 200

### T11.3 — Prometheus Metrics [P0] [HTTP]
- **Aktion:** `GET http://10.0.0.240:8080/metrics`
- **Erwartung:** Prometheus-Text-Format mit `cortex_requests_total`, `cortex_request_duration_seconds`
- **Pruefung:** Content-Type text/plain, Metriken vorhanden

### T11.4 — Control Plane Config GET [P0] [HTTP]
- **Aktion:** `GET http://10.0.0.240:8081/control/config`
- **Erwartung:** JSON mit primary_provider, temperature, max_tokens, rate_limit_rps
- **Pruefung:** Alle Felder vorhanden

### T11.5 — Control Plane Config PATCH [P1] [HTTP]
- **Aktion:** `PATCH http://10.0.0.240:8081/control/config` mit `{"temperature": 0.5}`
- **Erwartung:** HTTP 200, Config aktualisiert
- **Pruefung:** GET danach zeigt temperature=0.5
- **Cleanup:** PATCH zurueck auf Original-Wert

### T11.6 — Control Plane Temperature Validation [P1] [HTTP]
- **Aktion:** `PATCH` mit `{"temperature": -1.0}` und `{"temperature": 3.0}`
- **Erwartung:** HTTP 400 (Validation Error), Range [0.0, 2.0]
- **Pruefung:** Beide Requests abgelehnt

### T11.7 — Control Plane Provider Switch [P1] [HTTP]
- **Aktion:** `POST http://10.0.0.240:8081/control/provider` mit `{"provider":"ollama"}`
- **Erwartung:** HTTP 200, Provider gewechselt
- **Pruefung:** GET config zeigt neuen primary_provider
- **Cleanup:** Zurueck auf Original-Provider

### T11.8 — InFlightMap Metriken (Issue #99) [P1] [HTTP]
- **Aktion:** Prometheus Metrics abfragen
- **Erwartung:** `sentinel_query_inflight`, `sentinel_query_cancelled_total`, `sentinel_query_stale_dropped_total`
- **Pruefung:** Metriken existieren (Wert >= 0)

### T11.9 — Guardrails Endpoint (wenn aktiviert) [P1] [HTTP]
- **Aktion:** `GET http://10.0.0.240:8081/control/guardrails`
- **Erwartung:** Budget-Status, Rate-Limit-Info (oder 404 wenn deaktiviert)
- **Pruefung:** Valides JSON oder 404

### T11.10 — LLM Chat Completion [P0] [HTTP]
- **Aktion:** `POST http://10.0.0.240:8080/v1/chat/completions` mit Test-Payload
- **Erwartung:** HTTP 200, JSON mit `choices[0].message.content`
- **Pruefung:** Response hat sinnvollen Inhalt
- **ACHTUNG:** Verbraucht LLM-Tokens! Nur mit minimalem Payload testen.

---

## T12: NATS JetStream Infrastruktur (Issue #109)

### T12.1 — NATS Server Version [P0] [SSH]
- **Aktion:** `ssh ubuntu@10.0.0.240 "/opt/sentinel/bin/nats-server --version"`
- **Erwartung:** Version 2.12.4 oder hoeher
- **Pruefung:** Versionsnummer extrahieren

### T12.2 — SENTINEL_EVENTS Stream existiert [P0] [NATS]
- **Aktion:** `ssh ubuntu@10.0.0.240 "nats stream info SENTINEL_EVENTS"`
- **Erwartung:** Stream konfiguriert, 7-day retention, 1GB max
- **Pruefung:** Retention, MaxBytes korrekt

### T12.3 — SENTINEL_JUDGE Stream existiert [P0] [NATS]
- **Aktion:** `ssh ubuntu@10.0.0.240 "nats stream info SENTINEL_JUDGE"`
- **Erwartung:** Stream konfiguriert, 30-day retention, 100MB max
- **Pruefung:** Retention, MaxBytes korrekt

### T12.4 — judge-heuristic Consumer existiert [P0] [NATS]
- **Aktion:** `ssh ubuntu@10.0.0.240 "nats consumer info SENTINEL_EVENTS judge-heuristic"`
- **Erwartung:** Durable Pull Consumer aktiv
- **Pruefung:** Consumer-Type = Pull, Durable = true

### T12.5 — judge-batch Consumer existiert [P0] [NATS]
- **Aktion:** `ssh ubuntu@10.0.0.240 "nats consumer info SENTINEL_EVENTS judge-batch"`
- **Erwartung:** Durable Pull Consumer aktiv
- **Pruefung:** Analog T12.4

### T12.6 — Bridge publiziert Events [P0] [SSH]
- **Aktion:** `ssh ubuntu@10.0.0.240 "nats stream info SENTINEL_EVENTS"` — Messages-Count pruefen
- **Erwartung:** Messages > 0 (Bridge hat Events publiziert)
- **Pruefung:** Message-Count steigt ueber Zeit

### T12.7 — Bridge Subject-Pattern [P1] [SSH]
- **Aktion:** `ssh ubuntu@10.0.0.240 "nats stream subjects SENTINEL_EVENTS"`
- **Erwartung:** Subjects folgen Pattern `sentinel.events.{type}.{agent_id}`
- **Pruefung:** Mindestens 1 Subject mit korrektem Format

### T12.8 — NATS JetStream aktiviert [P0] [SSH]
- **Aktion:** NATS Monitoring `GET http://10.0.0.240:8222/jsz`
- **Erwartung:** JetStream-Info mit Streams-Count >= 2
- **Pruefung:** `streams >= 2`

### T12.9 — Bridge Dedup (Nats-Msg-Id) [P1] [SSH]
- **Aktion:** Gleiche Nachricht zweimal senden, Stream-Count pruefen
- **Erwartung:** Count steigt nur um 1 (Dedup verhindert Duplikat)
- **Pruefung:** Delta == 1

### T12.10 — Bridge Service stabil (kein Restart-Loop) [P0] [SSH]
- **Aktion:** `ssh ubuntu@10.0.0.240 "systemctl show sentinel-nats-bridge --property=NRestarts"`
- **Erwartung:** NRestarts == 0 oder sehr niedrig
- **Pruefung:** Kein Crash-Loop

---

## T13: Sentinel Judge (Issue #26)

### T13.1 — Judge Health [P0] [HTTP]
- **Aktion:** `GET http://10.0.0.240:8082/health`
- **Erwartung:** `{"status":"ok","service":"sentinel-judge"}`
- **Pruefung:** Status 200

### T13.2 — Judge Ready (NATS connected) [P0] [HTTP]
- **Aktion:** `GET http://10.0.0.240:8082/ready`
- **Erwartung:** `{"ready":true}` — bedeutet NATS Consumer aktiv
- **Pruefung:** ready === true

### T13.3 — Judge Prometheus Metrics [P1] [HTTP]
- **Aktion:** `GET http://10.0.0.240:8082/metrics`
- **Erwartung:** Prometheus-Format mit Judge-spezifischen Metriken
- **Pruefung:** Metriken vorhanden (drift, quality, fatigue, events, lag)

### T13.4 — Judge Batch Analyze Endpoint [P1] [HTTP]
- **Aktion:** `POST http://10.0.0.240:8082/api/v1/analyze` mit Test-Payload
- **Erwartung:** HTTP 200, Analysis-Response mit quality_score
- **Pruefung:** JSON-Schema korrekt
- **ACHTUNG:** Nutzt LLM-Tokens via Cortex Gateway!

### T13.5 — Judge Service stabil [P0] [SSH]
- **Aktion:** `ssh ubuntu@10.0.0.240 "systemctl show sentinel-judge --property=NRestarts"`
- **Erwartung:** NRestarts == 0
- **Pruefung:** Kein Crash-Loop

### T13.6 — Judge Alerts publiziert [P1] [NATS]
- **Aktion:** SENTINEL_JUDGE Stream Message-Count pruefen
- **Erwartung:** Messages >= 0 (kann 0 sein wenn keine Alerts)
- **Pruefung:** Stream existiert und ist konsumierbar

---

## T14: Sentinel Daemon (Issue #94, #107)

### T14.1 — Daemon Prozess aktiv [P0] [SSH]
- **Aktion:** `ssh ubuntu@10.0.0.240 "systemctl is-active sentinel-daemon"`
- **Erwartung:** "active"
- **Pruefung:** Output === "active"

### T14.2 — Daemon Uptime > 1h [P0] [SSH]
- **Aktion:** `ssh ubuntu@10.0.0.240 "systemctl show sentinel-daemon --property=ActiveEnterTimestamp"`
- **Erwartung:** Laeuft seit mindestens 1 Stunde (keine staendigen Restarts)
- **Pruefung:** Timestamp-Differenz > 3600s

### T14.3 — Events werden geschrieben [P0] [SSH]
- **Aktion:** `ssh ubuntu@10.0.0.240 "sqlite3 /opt/sentinel/data/events.db 'SELECT COUNT(*) FROM events'"`
- **Erwartung:** Count > 0 (Daemon produziert Events)
- **Pruefung:** Zahl > 0

### T14.4 — Event-Count steigt [P0] [SSH]
- **Aktion:** Count zweimal im Abstand von 5s messen
- **Erwartung:** Zweiter Count > erster Count (Daemon tickt aktiv)
- **Pruefung:** Delta > 0

### T14.5 — Daemon RAM-Verbrauch [P1] [SSH]
- **Aktion:** `ssh ubuntu@10.0.0.240 "ps -o rss= -p $(pgrep sentinel-daemon)"`
- **Erwartung:** < 200 MB (typisch ~62 MB nach 21h)
- **Pruefung:** RSS < 200000 KB

### T14.6 — Daemon kein Crash-Loop [P0] [SSH]
- **Aktion:** `ssh ubuntu@10.0.0.240 "systemctl show sentinel-daemon --property=NRestarts"`
- **Erwartung:** NRestarts == 0
- **Pruefung:** Kein Restart

### T14.7 — 54 Agent-Configs geladen [P1] [SSH]
- **Aktion:** `ssh ubuntu@10.0.0.240 "ls /opt/sentinel/config/agents/*.toml | wc -l"`
- **Erwartung:** 54 TOML-Dateien
- **Pruefung:** Count === 54

### T14.8 — Controlplane aktiv (Issue #107) [P1] [SSH]
- **Aktion:** Controlplane.toml auf VM pruefen
- **Erwartung:** `/opt/sentinel/config/controlplane.toml` existiert, cycle_interval konfiguriert
- **Pruefung:** File existiert, TOML parsebar

---

## T15: Release Manifest & Deploy (Issue #110, #28)

### T15.1 — Release Manifest existiert [P0] [CLI]
- **Aktion:** `cat deploy/release-manifest.json | jq '.artifacts | length'`
- **Erwartung:** 31 Artifacts gelistet
- **Pruefung:** Count === 31

### T15.2 — Manifest Schema valide [P0] [CLI]
- **Aktion:** `deploy/release-manifest.json` gegen `deploy/release-manifest.schema.json` validieren
- **Erwartung:** Schema-Validierung bestanden
- **Pruefung:** Kein Validation-Error

### T15.3 — SHA-256 Hashes vorhanden [P0] [CLI]
- **Aktion:** Alle `hash` Felder im Manifest pruefen
- **Erwartung:** Jedes Artifact hat `hash` Feld mit SHA-256 Format (64 hex chars)
- **Pruefung:** Regex `^[a-f0-9]{64}$` fuer jeden Hash

### T15.4 — 6 Binaries gelistet [P0] [CLI]
- **Aktion:** Binaries im Manifest filtern
- **Erwartung:** sentinel-daemon, sentinel-nightrun, sentinel-projection, cortex-gateway, sentinel-judge, sentinel-nats-bridge
- **Pruefung:** Alle 6 Namen vorhanden

### T15.5 — 10 Config-Files gelistet [P0] [CLI]
- **Aktion:** Configs im Manifest filtern
- **Erwartung:** rooms.toml, company.toml, simulation.toml, cortex-gateway.toml, nightrun.toml, observatory.toml, judge.toml, nats-bridge.toml, nats.conf, storage.toml
- **Pruefung:** Alle 10 vorhanden

### T15.6 — Systemd Units gelistet [P1] [CLI]
- **Aktion:** Systemd-Eintraege im Manifest filtern
- **Erwartung:** Mindestens 5 Service-Units
- **Pruefung:** sentinel-daemon.service, sentinel-cortex.service etc.

### T15.7 — Init Scripts gelistet [P1] [CLI]
- **Aktion:** Init-Script-Eintraege filtern
- **Erwartung:** 5 Scripts (dirs, tmpfs, cgroups, hugepages, sysctl)
- **Pruefung:** Alle 5 vorhanden

### T15.8 — Preflight Script funktioniert [P0] [CLI]
- **Aktion:** `deploy/deploy-preflight.sh` ausfuehren (dry-run oder gegen VM)
- **Erwartung:** Exit Code 0 bei Hash-Match, Exit Code 1 bei Mismatch
- **Pruefung:** Return Code korrekt

### T15.9 — Smoke Test Script [P0] [CLI]
- **Aktion:** `deploy/smoke-test.sh` oder `make smoke-test`
- **Erwartung:** Alle Health-Checks bestanden, Exit Code 0
- **Pruefung:** Alle Services erreichbar, korrekte Responses

### T15.10 — Makefile Targets existieren [P1] [CLI]
- **Aktion:** `make -n preflight`, `make -n deploy`, `make -n smoke-test`
- **Erwartung:** Alle 3 Targets definiert (dry-run erfolgreich)
- **Pruefung:** Kein "No rule to make target" Error

---

## T16: Konfiguration & Agent-Definitionen

### T16.1 — Alle Config-TOMLs parsebar [P0] [CLI]
- **Aktion:** Jede TOML-Datei in `config/` mit TOML-Parser pruefen
- **Erwartung:** Kein Parse-Error
- **Pruefung:** Exit Code 0 fuer alle

### T16.2 — 54 Agent-TOMLs vorhanden [P0] [CLI]
- **Aktion:** `ls agents/AGENT-*.toml | wc -l`
- **Erwartung:** 54
- **Pruefung:** Count === 54

### T16.3 — Agent-TOML Pflichtfelder [P0] [CLI]
- **Aktion:** Jedes Agent-TOML auf Pflichtfelder pruefen
- **Erwartung:** Jeder Agent hat: id, name, role, shift_set, personality.big_five
- **Pruefung:** Alle 5 Felder in jedem der 54 Files

### T16.4 — Schicht-Verteilung korrekt [P0] [CLI]
- **Aktion:** shift_set Werte aller 54 Agents zaehlen
- **Erwartung:** Schicht 1: 15, Schicht 2: 15, Schicht 3: 15, Schicht 0: 9
- **Pruefung:** Verteilung stimmt

### T16.5 — rooms.toml hat 17 Raeume [P0] [CLI]
- **Aktion:** `config/rooms.toml` parsen, Raum-Count pruefen
- **Erwartung:** 17 Raeume mit id, name, floor, capacity, room_type, adjacent
- **Pruefung:** Count === 17

### T16.6 — nats.conf valide [P0] [SSH]
- **Aktion:** `ssh ubuntu@10.0.0.240 "/opt/sentinel/bin/nats-server --config /etc/nats/nats.conf -t"`
- **Erwartung:** Config-Test bestanden
- **Pruefung:** Exit Code 0

### T16.7 — controlplane.toml vorhanden [P0] [CLI]
- **Aktion:** `config/controlplane.toml` lesen
- **Erwartung:** Existiert, hat cycle_interval, thresholds Sektion
- **Pruefung:** TOML parsebar, Pflichtfelder da

### T16.8 — storage.toml vorhanden [P0] [CLI]
- **Aktion:** `config/storage.toml` lesen
- **Erwartung:** Existiert, hat artifact-Sektion mit durability, chunk sizes
- **Pruefung:** TOML parsebar

### T16.9 — daemon.toml vorhanden [P0] [CLI]
- **Aktion:** `config/daemon.toml` lesen
- **Erwartung:** Existiert, hat tick_rate_ms, max_agents
- **Pruefung:** TOML parsebar

### T16.10 — simulation.toml vorhanden [P0] [CLI]
- **Aktion:** `config/simulation.toml` lesen
- **Erwartung:** Existiert, Shift-Model definiert
- **Pruefung:** TOML parsebar

---

## T17: Dokumentation (Issue #111)

### T17.1 — STATUS_MODEL.md existiert [P0] [CLI]
- **Aktion:** `docs/STATUS_MODEL.md` lesen
- **Erwartung:** Datei existiert, beschreibt Issue-Lifecycle
- **Pruefung:** Enthaelt "triage", "backlog", "ready", "in-progress", "review", "completed"

### T17.2 — DEFINITION_OF_DONE.md existiert [P0] [CLI]
- **Aktion:** `docs/DEFINITION_OF_DONE.md` lesen
- **Erwartung:** Datei existiert, definiert Feature-DoD und Gate-DoD
- **Pruefung:** Enthaelt "Feature DoD", "Gate DoD" oder aequivalent

### T17.3 — GATE_AUDIT existiert [P0] [CLI]
- **Aktion:** `docs/GATE_AUDIT_2026-02-20.md` lesen
- **Erwartung:** Datei existiert, dokumentiert Label-Audit
- **Pruefung:** Enthaelt Governance-Fixes

### T17.4 — CHANGELOG.md aktuell [P1] [CLI]
- **Aktion:** `CHANGELOG.md` lesen, letzten Eintrag pruefen
- **Erwartung:** Letzter Eintrag ist aktuell (Feb 2026)
- **Pruefung:** Datum-Eintrag nicht aelter als 1 Woche

---

## T18: CI/CD & Workflows

### T18.1 — 13 Workflows vorhanden [P0] [CLI]
- **Aktion:** `ls .github/workflows/*.yml | wc -l`
- **Erwartung:** >= 13 Workflow-Dateien
- **Pruefung:** Count >= 13

### T18.2 — main-push-guard aktiv [P0] [CLI]
- **Aktion:** `.github/workflows/main-push-guard.yml` lesen
- **Erwartung:** Workflow blockiert direkte Pushes auf main
- **Pruefung:** Datei existiert, on: push: branches: [main]

### T18.3 — CI Workflow fuer PRs [P0] [CLI]
- **Aktion:** CI-Workflow fuer Pull Requests pruefen
- **Erwartung:** Lint, Test, Clippy, Format-Checks enthalten
- **Pruefung:** Jobs definiert

### T18.4 — Release Workflow [P1] [CLI]
- **Aktion:** `release.yml` Workflow pruefen
- **Erwartung:** Generiert Release-Manifest, SBOM als Artifact
- **Pruefung:** Steps fuer Manifest + SBOM vorhanden

---

## T19: Sentinel-FS Artifact Plane (Issue #56)

### T19.1 — sentinel-fs Crate kompiliert [P0] [CLI]
- **Aktion:** `cargo remote -- build -p sentinel-fs`
- **Erwartung:** Build erfolgreich, keine Errors
- **Pruefung:** Exit Code 0

### T19.2 — sentinel-fs Tests bestehen [P0] [CLI]
- **Aktion:** `cargo remote -- test -p sentinel-fs`
- **Erwartung:** Alle 87+ Tests bestanden
- **Pruefung:** "test result: ok", 0 failed

### T19.3 — CDC Chunking deterministisch [P0] [CLI]
- **Aktion:** Gleiche Datei zweimal chunken, Chunk-Hashes vergleichen
- **Erwartung:** Identische Hashes bei identischem Input
- **Pruefung:** Alle Chunk-IDs identisch

### T19.4 — Dedup-Effektivitaet [P0] [CLI]
- **Aktion:** Identische Datei 2x ingestieren, neue Chunks zaehlen
- **Erwartung:** 0 neue Chunks beim 2. Ingest
- **Pruefung:** Delta Chunks == 0

### T19.5 — Multi-Format Ingest [P1] [CLI]
- **Aktion:** .bin, .html, .pdf, .json Dateien ingestieren
- **Erwartung:** Alle 4 Formate erfolgreich verarbeitet
- **Pruefung:** Kein Error, alle Objects abrufbar

### T19.6 — Refcount GC [P1] [CLI]
- **Aktion:** Object erstellen, loeschen, GC ausfuehren
- **Erwartung:** Orphan-Chunks aufgeraeumt, 0 Orphans nach GC
- **Pruefung:** gc_chunks() meldet 0 Orphans

### T19.7 — Transaktionale Atomizitaet [P0] [CLI]
- **Aktion:** Ingest starten, abort aufrufen
- **Erwartung:** 0 DB-Artefakte nach abort (kein Datenmull)
- **Pruefung:** Keine neuen Chunks/Objects in DB

### T19.8 — L1 Cache Stats [P1] [CLI]
- **Aktion:** cache_stats() nach Reads pruefen
- **Erwartung:** Hits, Misses, Entries, Bytes korrekt gezaehlt
- **Pruefung:** Stats-Werte plausibel

### T19.9 — Batch Ingest Performance [P1] [CLI]
- **Aktion:** 10x 100KB Dateien per Batch-API ingestieren
- **Erwartung:** < 150ms Gesamtzeit
- **Pruefung:** Benchmark-Ergebnis unter Threshold

### T19.10 — storage.toml Config geladen [P0] [CLI]
- **Aktion:** sentinel-fs mit config/storage.toml starten
- **Erwartung:** Alle Konfigurationswerte (chunk sizes, durability, IOPS) korrekt geladen
- **Pruefung:** Kein Config-Error

### T19.11 — Segment-Pack Storage [P1] [CLI]
- **Aktion:** Nach Ingest Segment-Files pruefen
- **Erwartung:** .seg Dateien im Data-Verzeichnis, ~64MB Segments
- **Pruefung:** Dateien existieren

### T19.12 — Clippy clean [P0] [CLI]
- **Aktion:** `cargo remote -- clippy -p sentinel-fs -- -D warnings`
- **Erwartung:** 0 Warnings
- **Pruefung:** Exit Code 0

---

## T20: Sentinel Nightrun (Memory Consolidation)

### T20.1 — Nightrun Dry-Run [P0] [CLI]
- **Aktion:** `sentinel-nightrun --config config/nightrun.toml --dry-run`
- **Erwartung:** Listet Agents der aktuellen Schicht, Exit Code 0
- **Pruefung:** Output zeigt Agent-Liste

### T20.2 — Nightrun Config parsebar [P0] [CLI]
- **Aktion:** `config/nightrun.toml` lesen
- **Erwartung:** Valide TOML mit hippocampus_db, event_store_db, timeout Felder
- **Pruefung:** Alle Pflichtfelder vorhanden

### T20.3 — Nightrun systemd Timer [P1] [SSH]
- **Aktion:** `ssh ubuntu@10.0.0.240 "systemctl list-timers | grep nightrun"`
- **Erwartung:** Timer aktiv (06:00, 14:00, 22:00 UTC)
- **Pruefung:** Timer gelistet mit naechstem Ausfuehrungszeitpunkt

---

## T20a: Projection Worker (CQRS Read-Model)

### T20a.1 — Projection DB Tabellen existieren [P0] [SSH]
- **Aktion:** `ssh ubuntu@10.0.0.240 "python3 -c \"import sqlite3; c=sqlite3.connect('/opt/sentinel/data/projection.db'); print([r[0] for r in c.execute('SELECT name FROM sqlite_master WHERE type=\\'table\\'')]); c.close()\""`
- **Erwartung:** Tabellen `agent_live_view`, `room_live_view`, `kpi_1m` vorhanden
- **Pruefung:** Alle 3 Tabellen in der Liste

### T20a.2 — room_live_view hat 17 Raeume [P0] [SSH]
- **Aktion:** `ssh ubuntu@10.0.0.240 "python3 -c \"import sqlite3; c=sqlite3.connect('/opt/sentinel/data/projection.db'); print(c.execute('SELECT COUNT(*) FROM room_live_view').fetchone()[0]); c.close()\""`
- **Erwartung:** 17 Eintraege (alle Raeume)
- **Pruefung:** Count === 15

### T20a.3 — kpi_1m wird befuellt [P0] [SSH]
- **Aktion:** kpi_1m Count pruefen, 30s warten, erneut pruefen
- **Erwartung:** Count steigt (Worker verarbeitet Events kontinuierlich)
- **Pruefung:** Delta > 0

### T20a.4 — Projection Worker stabil [P0] [SSH]
- **Aktion:** `ssh ubuntu@10.0.0.240 "systemctl show sentinel-projection --property=NRestarts"`
- **Erwartung:** NRestarts == 0
- **Pruefung:** Kein Crash-Loop

---

## T20b: Outbox Drain (Transactional Outbox Pattern)

### T20b.1 — Outbox pending bei 0 [P0] [SSH]
- **Aktion:** `ssh ubuntu@10.0.0.240 "python3 -c \"import sqlite3; c=sqlite3.connect('/opt/sentinel/data/events.db'); print(c.execute('SELECT COUNT(*) FROM outbox WHERE status=\\'pending\\'').fetchone()[0]); c.close()\""`
- **Erwartung:** 0 oder sehr wenige pending Eintraege (Bridge draint aktiv)
- **Pruefung:** Count < 100

### T20b.2 — Outbox published Count > 0 [P0] [SSH]
- **Aktion:** published Count in outbox pruefen
- **Erwartung:** > 200000 (historische + neue Events)
- **Pruefung:** Count > 200000

### T20b.3 — Outbox failed bei 0 [P0] [SSH]
- **Aktion:** failed Count in outbox pruefen
- **Erwartung:** 0 failed Eintraege (keine NATS-Probleme)
- **Pruefung:** Count === 0

### T20b.4 — Outbox retry_count Spalte vorhanden [P0] [SSH]
- **Aktion:** `PRAGMA table_info(outbox)` pruefen
- **Erwartung:** Spalten retry_count und last_error vorhanden
- **Pruefung:** Beide Spalten in Schema

---

## T21: End-to-End Flow Tests (Integration)

### T21.1 — Event Flow: Daemon → Bridge → NATS [P0] [SSH+NATS]
- **Aktion:**
  1. Event-Count in events.db notieren
  2. NATS SENTINEL_EVENTS Message-Count notieren
  3. 10s warten
  4. Beide Counts erneut pruefen
- **Erwartung:** Beide Counts gestiegen, Delta aehnlich (Bridge uebersetzt Events)
- **Pruefung:** events.db Delta ~ NATS Delta (Toleranz: Batch-Verzoegerung)

### T21.2 — Dashboard zeigt Live-Daten [P0] [PW]
- **Aktion:**
  1. Dashboard oeffnen
  2. Agents-View: Mindestens 1 aktiver Agent
  3. Metriken-View: Uptime > 0
  4. Cockpit-View: SLO-Leiste sichtbar
- **Erwartung:** Echte Daten (keine leeren Views)
- **Pruefung:** Numerische Werte > 0

### T21.3 — Dashboard reagiert auf Events [P0] [PW]
- **Aktion:**
  1. Dashboard oeffnen, Agent-Count notieren
  2. 30s warten
  3. Agent-Count erneut pruefen (oder Activity-View auf neue Eintraege)
- **Erwartung:** Daten aktualisieren sich (WebSocket-Updates)
- **Pruefung:** Mindestens 1 DOM-Update beobachtet

### T21.4 — Cortex Gateway verarbeitet Anfragen [P0] [HTTP]
- **Aktion:** Minimalen Chat-Completion Request senden
- **Erwartung:** LLM-Antwort zurueck, Pipeline durchlaufen
- **Pruefung:** Response hat choices[0].message.content
- **ACHTUNG:** Token-Verbrauch!

### T21.5 — Judge konsumiert Events [P1] [SSH+NATS]
- **Aktion:** Judge consumer-info pruefen, processed Messages > 0
- **Erwartung:** Consumer hat Messages verarbeitet (ack'd)
- **Pruefung:** `delivered.stream_seq > 0`

---

## T22: Security & Hardening

### T22.1 — Kein innerHTML im Frontend-Code [P0] [CLI]
- **Aktion:** `grep -r "innerHTML" dashboard/public/js/`
- **Erwartung:** 0 Treffer
- **Pruefung:** Exit Code 1 (kein Match)

### T22.2 — Keine Secrets in Git [P0] [CLI]
- **Aktion:** `git log --all -p | grep -iE "(api.key|secret|password|token)" | head -20`
- **Erwartung:** Keine echten Credentials in Git-History
- **Pruefung:** Keine Treffer oder nur Platzhalter

### T22.3 — .gitignore schuetzt Secrets [P0] [CLI]
- **Aktion:** `.gitignore` pruefen
- **Erwartung:** `.env`, `*.key`, `credentials*` enthalten
- **Pruefung:** Patterns vorhanden

### T22.4 — NATS nur localhost [P0] [SSH]
- **Aktion:** NATS Config `listen` Direktive pruefen
- **Erwartung:** `127.0.0.1:4222` (nicht 0.0.0.0)
- **Pruefung:** nats.conf bind-Adresse

### T22.5 — systemd Security-Hardening [P1] [SSH]
- **Aktion:** Service-Files auf Security-Direktiven pruefen
- **Erwartung:** ProtectSystem, NoNewPrivileges, MemoryLimit gesetzt
- **Pruefung:** Direktiven vorhanden

---

## T23: Bio-Bar Ranges (Agent-Vitalwerte)

### T23.1 — Bio-Felder in API vorhanden [P0] [HTTP]
- **Aktion:** `GET /api/agents`, Response-Schema pruefen
- **Erwartung:** Jeder Agent hat Felder: `hunger`, `energy`, `stress`, `bladder`, `social_need`, `caffeine_mg`
- **Pruefung:** Alle 6 Bio-Felder existieren in jedem Agent-Objekt

### T23.2 — Hunger Range [0.0, 1.0] [P0] [HTTP]
- **Aktion:** Alle `hunger` Werte aus `/api/agents` extrahieren
- **Erwartung:** Jeder Wert >= 0.0 und <= 1.0
- **Pruefung:** `0.0 <= hunger <= 1.0` fuer jeden Agent

### T23.3 — Energy Range [0.0, 1.0] [P0] [HTTP]
- **Aktion:** Alle `energy` Werte aus `/api/agents` extrahieren
- **Erwartung:** Jeder Wert >= 0.0 und <= 1.0
- **Pruefung:** `0.0 <= energy <= 1.0` fuer jeden Agent

### T23.4 — Stress Range [0.0, 1.0] [P0] [HTTP]
- **Aktion:** Alle `stress` Werte aus `/api/agents` extrahieren
- **Erwartung:** Jeder Wert >= 0.0 und <= 1.0
- **Pruefung:** `0.0 <= stress <= 1.0` fuer jeden Agent

### T23.5 — Bladder Range [0.0, 1.0] [P0] [HTTP]
- **Aktion:** Alle `bladder` Werte aus `/api/agents` extrahieren
- **Erwartung:** Jeder Wert >= 0.0 und <= 1.0
- **Pruefung:** `0.0 <= bladder <= 1.0` fuer jeden Agent

### T23.6 — Social Need Range [0.0, 1.0] [P0] [HTTP]
- **Aktion:** Alle `social_need` Werte aus `/api/agents` extrahieren
- **Erwartung:** Jeder Wert >= 0.0 und <= 1.0
- **Pruefung:** `0.0 <= social_need <= 1.0` fuer jeden Agent

### T23.7 — Caffeine Range [0.0, 1.0] [P0] [HTTP]
- **Aktion:** Alle `caffeine_mg` Werte aus `/api/agents` extrahieren
- **Erwartung:** Jeder Wert >= 0.0 und <= 1.0
- **Pruefung:** `0.0 <= caffeine_mg <= 1.0` fuer jeden Agent

### T23.8 — Bio-Werte numerisch (kein NaN/Infinity) [P0] [HTTP]
- **Aktion:** Alle Bio-Felder pruefen
- **Erwartung:** Alle Werte sind endliche Zahlen (kein NaN, kein Infinity, kein null)
- **Pruefung:** `typeof val === 'number' && isFinite(val)` fuer alle 6 Felder

### T23.9 — Mood-Feld vorhanden [P1] [HTTP]
- **Aktion:** `mood` Feld jedes Agents pruefen
- **Erwartung:** String oder null (z.B. "neutral", "happy", "stressed")
- **Pruefung:** `typeof mood === 'string' || mood === null`

### T23.10 — Bio-Defaults bei Spawn korrekt [P0] [HTTP]
- **Aktion:** Agent-Detail `/api/agents/:id/state` fuer neu gespawnten Agent pruefen
- **Erwartung:** Default-Werte: hunger=0, energy=1, stress=0, bladder=0, social_need=0, caffeine_mg=0
- **Pruefung:** Exakte Default-Werte bei frischem Agent

---

## T24: Room Physics Format

### T24.1 — Physics-Felder in API vorhanden [P0] [HTTP]
- **Aktion:** `GET /api/rooms`, Response-Schema pruefen
- **Erwartung:** Jeder Raum hat Felder: `temperature`, `co2_ppm`, `noise_db`
- **Pruefung:** Alle 3 Physics-Felder existieren (Wert oder null)

### T24.2 — Temperatur plausibel [P0] [HTTP]
- **Aktion:** Alle `temperature` Werte aus `/api/rooms` extrahieren (nicht-null)
- **Erwartung:** Bereich 15.0-35.0 Grad Celsius (Buero-Range)
- **Pruefung:** `15.0 <= temperature <= 35.0` fuer jeden Raum mit Wert

### T24.3 — Noise dB plausibel [P0] [HTTP]
- **Aktion:** Alle `noise_db` Werte aus `/api/rooms` extrahieren (nicht-null)
- **Erwartung:** Bereich 20.0-90.0 dB (Buero-Pegel: Fluestern bis lauter Grossraum)
- **Pruefung:** `20.0 <= noise_db <= 90.0` fuer jeden Raum mit Wert

### T24.4 — CO2 ppm plausibel [P0] [HTTP]
- **Aktion:** Alle `co2_ppm` Werte aus `/api/rooms` extrahieren (nicht-null)
- **Erwartung:** Bereich 350-3000 ppm (Aussenluft bis schlechte Raumluft)
- **Pruefung:** `350 <= co2_ppm <= 3000` fuer jeden Raum mit Wert

### T24.5 — Physics-Werte numerisch [P0] [HTTP]
- **Aktion:** Alle nicht-null Physics-Werte pruefen
- **Erwartung:** Endliche Zahlen (kein NaN/Infinity)
- **Pruefung:** `typeof val === 'number' && isFinite(val)`

### T24.6 — Besetzte Raeume haben Physics-Werte [P1] [HTTP]
- **Aktion:** Raeume mit `occupant_count > 0` pruefen
- **Erwartung:** temperature, co2_ppm, noise_db sind nicht-null
- **Pruefung:** Alle 3 Werte vorhanden bei besetzten Raeumen

### T24.7 — CO2 steigt mit Belegung [P1] [HTTP]
- **Aktion:** co2_ppm von leeren vs. belegten Raeumen vergleichen
- **Erwartung:** Belegte Raeume tendieren zu hoeherem CO2
- **Pruefung:** Durchschnitt(belegt) >= Durchschnitt(leer) (wenn Daten vorhanden)

### T24.8 — Noise steigt mit Belegung [P1] [HTTP]
- **Aktion:** noise_db von leeren vs. belegten Raeumen vergleichen
- **Erwartung:** Belegte Raeume tendieren zu hoeherer Lautstaerke
- **Pruefung:** Durchschnitt(belegt) >= Durchschnitt(leer) (wenn Daten vorhanden)

---

## T25: Chaos-Event-Typen (spezifisch statt generisch)

### T25.1 — 8 Chaos-Typen definiert [P0] [HTTP]
- **Aktion:** `GET /api/chaos?limit=1000`, alle `chaos_type` Werte sammeln
- **Erwartung:** Nur Werte aus dem Set: PhoneRing, PrinterBroken, PackageDelivery, SBahnDelay, FireAlarmDrill, CakeInKitchen, AirConBroken, InternetOutage
- **Pruefung:** `chaos_type in VALID_TYPES` fuer jeden Event

### T25.2 — Kein generisches "ChaosTriggered" [P0] [HTTP]
- **Aktion:** Alle `chaos_type` Werte pruefen
- **Erwartung:** KEIN Event hat `chaos_type === "ChaosTriggered"` oder `chaos_type === "chaos_triggered"`
- **Pruefung:** Kein einziger generischer Typ in der Response

### T25.3 — Kein "unknown" Chaos-Typ [P0] [HTTP]
- **Aktion:** Alle `chaos_type` Werte pruefen
- **Erwartung:** KEIN Event hat `chaos_type === "unknown"`
- **Pruefung:** Payload-Parsing funktioniert korrekt

### T25.4 — Chaos-Events haben Pflichtfelder [P0] [HTTP]
- **Aktion:** Jedes Chaos-Event-Objekt pruefen
- **Erwartung:** Felder `id`, `event_id`, `chaos_type`, `room_id`, `description`, `tick`, `timestamp_ms` vorhanden
- **Pruefung:** Alle 7 Pflichtfelder existieren

### T25.5 — Chaos room_id ist valider Raum [P0] [HTTP]
- **Aktion:** `room_id` jedes Chaos-Events gegen `/api/rooms` Liste pruefen
- **Erwartung:** Jede room_id existiert in der Room-Liste (oder ist null)
- **Pruefung:** Cross-Referenz Rooms ↔ Chaos Events

### T25.6 — Chaos tick monoton steigend [P1] [HTTP]
- **Aktion:** Events nach ID sortiert, Tick-Werte pruefen
- **Erwartung:** Spaetere Events haben >= Tick (monoton)
- **Pruefung:** `events[i].tick <= events[i+1].tick` fuer benachbarte Events

### T25.7 — Chaos description nicht leer [P0] [HTTP]
- **Aktion:** `description` Feld jedes Events pruefen
- **Erwartung:** Nicht-leerer String mit deutschem Beschreibungstext
- **Pruefung:** `description.length > 0`

### T25.8 — Chaos timestamp_ms plausibel [P1] [HTTP]
- **Aktion:** `timestamp_ms` Werte pruefen
- **Erwartung:** Alle Timestamps > 0 und <= aktuelle Zeit
- **Pruefung:** `0 < timestamp_ms <= Date.now()`

---

## T26: Cockpit Incidents Lifecycle

### T26.1 — Incident Status-Werte gueltig [P0] [HTTP]
- **Aktion:** `GET /api/cockpit`, alle `incidents[].status` pruefen
- **Erwartung:** Nur Werte aus: "active", "resolved", "pending", "failed"
- **Pruefung:** `status in VALID_STATUSES`

### T26.2 — Incident Severity-Werte gueltig [P0] [HTTP]
- **Aktion:** Alle `incidents[].severity` pruefen
- **Erwartung:** Nur Werte aus: "critical", "high", "medium", "low"
- **Pruefung:** `severity in VALID_SEVERITIES`

### T26.3 — Aktive Incidents Count stimmt [P0] [HTTP]
- **Aktion:** `total_active` mit tatsaechlicher Anzahl status=active vergleichen
- **Erwartung:** `total_active === incidents.filter(i => i.status === 'active').length`
- **Pruefung:** Konsistenz-Check

### T26.4 — Resolved Count plausibel [P0] [HTTP]
- **Aktion:** `total_resolved_24h` pruefen
- **Erwartung:** Numerisch >= 0
- **Pruefung:** `total_resolved_24h >= 0`

### T26.5 — Incident hat Pflichtfelder [P0] [HTTP]
- **Aktion:** Jedes Incident-Objekt inspizieren
- **Erwartung:** Felder `id`, `source`, `incident_type`, `severity`, `status`, `summary`, `tick`, `timestamp_ms`, `actions`, `outcome` vorhanden
- **Pruefung:** Alle 10 Pflichtfelder existieren

### T26.6 — Incident Source gueltig [P0] [HTTP]
- **Aktion:** Alle `incidents[].source` pruefen
- **Erwartung:** Nur "event" oder "evolution"
- **Pruefung:** `source in ["event", "evolution"]`

### T26.7 — Incident Actions Array [P0] [HTTP]
- **Aktion:** Alle `incidents[].actions` pruefen
- **Erwartung:** Array (kann leer sein), jede Action hat `event_id`, `event_type`, `agent_id`, `summary`, `tick`
- **Pruefung:** Schema-Validierung der Action-Objekte

### T26.8 — Incident Auto-Resolve nach 30 Minuten [P1] [HTTP]
- **Aktion:** Incidents mit `outcome === "Automatisch abgeschlossen"` finden
- **Erwartung:** Diese Incidents haben status "resolved" und sind > 30min alt
- **Pruefung:** Auto-Resolve-Logik funktioniert korrekt

### T26.9 — SLO Violations Schema [P0] [HTTP]
- **Aktion:** `slo_violations` Array pruefen
- **Erwartung:** Jedes SLO hat `name`, `current_value`, `threshold`, `severity`, `description`
- **Pruefung:** Alle 5 Felder vorhanden

### T26.10 — SLO Threshold-Werte korrekt [P0] [HTTP]
- **Aktion:** Bekannte SLO-Thresholds pruefen
- **Erwartung:** Projection Lag=100, Chaos-Frequenz=3, Despawn-Rate=2, Nightrun Failure=10
- **Pruefung:** Threshold-Werte matchen Konfiguration

### T26.11 — Incident-Detail via ID abrufbar [P0] [HTTP]
- **Aktion:** Erste Incident-ID nehmen, `GET /api/cockpit/incident/:id`
- **Erwartung:** Gleiches Incident-Objekt mit vollstaendigen Daten
- **Pruefung:** Response-Schema identisch, alle Felder vorhanden

### T26.12 — Cockpit hours-Parameter filtert korrekt [P1] [HTTP]
- **Aktion:** `/api/cockpit?hours=1` vs `/api/cockpit?hours=168`
- **Erwartung:** hours=1 liefert weniger oder gleich viele Incidents wie hours=168
- **Pruefung:** `count(hours=1) <= count(hours=168)`

---

## Zusammenfassung

| Kategorie | Anzahl Tests | P0 | P1 | P2 |
|-----------|-------------|-----|-----|-----|
| T1: Infrastruktur Health | 15 | 14 | 1 | 0 |
| T2: Dashboard Navigation | 14 | 11 | 2 | 1 |
| T3: Dashboard Agents | 9 | 6 | 3 | 0 |
| T4: Dashboard Floorplan | 15 | 7 | 8 | 0 |
| T5: Dashboard Activity | 6 | 3 | 3 | 0 |
| T5a: Dashboard Chaos Feed | 7 | 4 | 3 | 0 |
| T5b: Dashboard Chat | 5 | 3 | 2 | 0 |
| T6: Dashboard Metriken | 10 | 7 | 3 | 0 |
| T7: Dashboard Cockpit | 17 | 11 | 6 | 0 |
| T8: Dashboard API | 14 | 11 | 3 | 0 |
| T9: Dashboard WebSocket | 7 | 5 | 2 | 0 |
| T10: Dashboard Styling | 4 | 1 | 2 | 1 |
| T11: Cortex Gateway | 10 | 4 | 6 | 0 |
| T12: NATS Infrastruktur | 10 | 7 | 3 | 0 |
| T13: Sentinel Judge | 6 | 3 | 3 | 0 |
| T14: Sentinel Daemon | 8 | 5 | 3 | 0 |
| T15: Release Manifest | 10 | 6 | 4 | 0 |
| T16: Konfiguration | 10 | 8 | 2 | 0 |
| T17: Dokumentation | 4 | 3 | 1 | 0 |
| T18: CI/CD | 4 | 2 | 2 | 0 |
| T19: Sentinel-FS | 12 | 6 | 6 | 0 |
| T20: Nightrun | 3 | 1 | 2 | 0 |
| T20a: Projection Worker | 4 | 4 | 0 | 0 |
| T20b: Outbox Drain | 4 | 4 | 0 | 0 |
| T21: E2E Flow | 5 | 4 | 1 | 0 |
| T22: Security | 5 | 3 | 2 | 0 |
| T23: Bio-Bar Ranges | 10 | 8 | 2 | 0 |
| T24: Room Physics Format | 8 | 5 | 3 | 0 |
| T25: Chaos-Event-Typen | 8 | 6 | 2 | 0 |
| T26: Cockpit Incidents Lifecycle | 12 | 9 | 3 | 0 |
| **TOTAL** | **256** | **183** | **85** | **2** |

### Ausfuehrungsreihenfolge
1. **T1** (Health) — Gate: Wenn ein T1-Test failt, ALLE anderen Tests abbrechen
2. **T16** (Config) — Sicherstellen dass alle Configs valide sind
3. **T14** (Daemon) — Kern-Service laeuft
4. **T12** (NATS) — Message-Bus funktioniert
5. **T8** (API) — Dashboard-Backend erreichbar
6. **T23** (Bio-Bar) — Agent-Vitalwerte validieren
7. **T24** (Room Physics) — Raum-Physik-Werte validieren
8. **T25** (Chaos-Typen) — Spezifische Chaos-Typen pruefen
9. **T2-T7, T9-T10** (Dashboard UI) — Frontend-Tests
10. **T26** (Cockpit Lifecycle) — Incident-Lifecycle validieren
11. **T11** (Cortex) — LLM-Pipeline
12. **T13** (Judge) — Quality-Service
13. **T15** (Manifest) — Deployment-Artefakte
14. **T17-T18** (Docs/CI) — Governance
15. **T19** (FS) — Artifact Plane
16. **T20** (Nightrun) — Memory Consolidation
17. **T21** (E2E Flow) — Integration
18. **T22** (Security) — Haertung

### Exit-Kriterien fuer Release
- **MUST:** Alle P0 Tests bestanden (183 Tests)
- **SHOULD:** Alle P1 Tests bestanden (85 Tests)
- **MAY:** P2 Tests koennen nachgereicht werden (2 Tests)
- **Release-Blocker:** Jeder P0-Fail stoppt das Release
