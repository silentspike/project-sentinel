# PROGRESS

## Status

- Issue: `#289` Room-Kommunikation Phase 2
- Overall status: `IN_PROGRESS`
- Current task: `7. 17/17 ACs mit frischer Evidence verifizieren`
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
- Die Deploy-VM `10.0.0.240` ist wieder erreichbar; `sentinel-daemon`, `sentinel-projection` und `sentinel-gateway` liefen beim Re-Check alle `active`.
- Die Runtime ist fuer Repro stabiler als erwartet, weil `localhost:8081/control/config` aktuell bereits `rate_limit_rps=0` meldet. Normale LLM-Autonomie ist damit pausiert.
- Gaia ist ein belastbarer MO-Trigger: `operator_gaia_sent -> agent_action_received(Move) -> transit_started` ist live fuer `AGENT-11`, `AGENT-46..50` belegt.
- `buero-betriebsarzt` ist als MO-Repro-Raum bestaetigt: live von `occupant_count=3` auf `occupant_count=6` gebracht, waehrend `transit_count` separat sichtbar blieb.
- `MO6`-relevante Trennung ist sowohl live als auch im Code bestaetigt: `decision.rs` zaehlt nur stationaere oder `transit_paused` Agents zur Occupancy.
- Wahrscheinliche Spec-/Code-Diskrepanz fuer `MO2`: `generate_capacity_events()` feuert `"Der Raum ist komplett voll."` bereits bei `occupancy >= capacity + 2`, nicht erst darueber.
- Room-ID-Drift ist fuer operatornahe Fixtures, Dashboard-Optionen, Daemon-/Common-/Zenoh-/Wasm-Tests und Design-Agent-Spawn-Configs bereinigt.
- Die verbliebenen `rg`-Treffer auf Legacy-Room-IDs liegen jetzt nur noch in historischen Notizen oder bewusst erhaltenen Alias-Resolvern.
- Remote-Regressionen fuer den Task-4-Scope sind gruen: `sentinel-common`, `sentinel-daemon`, `sentinel-zenoh`, `sentinel-wasm` sowie die betroffenen Dashboard-Tests.
- Ein wiederholbarer Room-Phase-2-Benchmark-Harness existiert jetzt in `sentinel-ecs` und laeuft remote ueber `cargo remote -c -- bench -p sentinel-ecs --bench room_phase2_bench`.
- Frischer Benchmark-Lauf liegt klar innerhalb der Pflichtbudgets:
  Route-BFS `2.53-5.15us`, Encounter-Detection realistisch `7.25us` fuer `26` Agents, Room-Phase-2-Tick `156.77us`.
- Ein dichter Stressfall fuer `encounter_system()` mit `26` gleichzeitig transitierten Agents im selben Flur liegt bei `202.62-209.46us`; das ist kein PFLICHT-Zielwert aus dem Issue, bleibt aber als Worst-Case-Evidence festgehalten.

## Blocked items

- Kein externer Infrastruktur-Blocker mehr aktiv.

## Task table

| # | Task | Status | Scope | Evidence |
|---|------|--------|-------|----------|
| 1 | Baseline bestaetigen | DONE | Commit-Basis, Issue-SSOT, Branch/Worktree, Runtime-Status | command |
| 2 | GitHub-AC-Matrix erstellen | DONE | 17 ACs in pruefbare Matrix mit Evidence-Mapping ueberfuehren | command, inspect |
| 3 | MO1-MO6 im laufenden System reproduzierbar machen | DONE | reproduzierbare Operator-/API-Trigger fuer Kapazitaetstests | command, system |
| 4 | Verbleibende Code-Luecken schliessen | DONE | nur die real offenen Ursachen beheben | command, inspect, system |
| 5 | Benchmarks implementieren oder an vorhandene Harnesses anbinden | DONE | BFS-, Encounter- und Tick-Benchmarkpfade absichern | command |
| 6 | TOGAF aktualisieren | TODO | Transit-Zeiten auf `15s-120s` angleichen | inspect, command |
| 7 | 17/17 ACs mit frischer Evidence verifizieren | IN_PROGRESS | jede AC einzeln im laufenden System oder passendem Harness nachweisen | command, system, inspect |
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

## Task 3 evidence summary

- VM-Reachability wiederhergestellt:
  `ssh -o ConnectTimeout=5 ubuntu@10.0.0.240 "hostname && systemctl is-active sentinel-daemon && systemctl is-active sentinel-projection && systemctl is-active sentinel-gateway"`
  Ergebnis: Host erreichbar, alle drei Dienste `active`.
- Runtime-API verfuegbar:
  `/api/agents` lieferte `26` Agents, `/api/rooms` lieferte `26` Raeume.
- Repro-Raum identifiziert:
  `buero-betriebsarzt` startete mit `capacity=4`, `occupant_count=3`, `transit_count=0`.
- Gaia-Trigger fuer Einzelmove bestaetigt:
  `AGENT-11 -> buero-it` fuehrte live zu `operator_gaia_sent`, `agent_action_received|Move|buero-it`, `transit_started` und `in_transit=true`.
- MO1/MO2-Setup live etabliert:
  `AGENT-46`, `47`, `48`, `49` wurden per Gaia nach `buero-betriebsarzt` geschickt.
  Zwischenstand belegte `occupant_count=4`, spaeter `occupant_count=6` bei gleichzeitigem `transit_count=5`.
- Encounter-bedingte Verzoegerung nachvollziehbar:
  Event-Store zeigte `hallway_encounter_detected` fuer `AGENT-46-49` und `AGENT-46-48`, wodurch die restlichen Agenten zunaechst im `flur-og` pausierten.
- MO3-Repro bestaetigt:
  Trotz bereits ueberbelegtem `buero-betriebsarzt` wurde `AGENT-50` erneut nach dorthin geschickt.
  Event-Store zeigte wieder `operator_gaia_sent`, `agent_action_received|Move|buero-betriebsarzt`, `transit_started`, also keinen Hard-Block.
- MO6-Repro bestaetigt:
  Live-API zeigte im selben Raum getrennte Werte fuer `occupant_count` und `transit_count`.
  Codepfad in `crates/sentinel-ecs/src/decision.rs` bildet `room_occupancy` nur aus `!in_transit || transit_paused`.

## Task 4 evidence summary

- Operator- und Dashboard-Room-IDs auf reale `rooms.toml`-Werte gezogen:
  `dashboard/public/operator.html` nutzt jetzt `buero-design-1`, `kueche`, `meetingraum-01`; die betroffenen Dashboard-Tests sind entsprechend aktualisiert.
- Direkte Runtime-Drift in Design-Agent-Configs behoben:
  `favorite_room = "buero-design"` wurde in sechs Agent-Dateien auf reale OG-Raeume (`buero-design-1` / `buero-design-2`) umgestellt.
  Die Spawn-Pfade in Daemon/ECS nutzen diese IDs direkt, also war das keine reine Testkosmetik.
- Daemon-/Common-/Zenoh-/Wasm-Fixtures auf echte Room-IDs normalisiert:
  `kueche-eg -> kueche`, `konferenz-1 -> meetingraum-01`, `buero-design -> buero-design-1`.
- Nachbereinigung bestaetigt:
  `rg -n "konferenz-1|kueche-eg|toilette-eg|favorite_room = \"buero-design\"|R:buero-design\\b|room_id: \"buero-design\"" /work/company/project-sentinel`
  laesst nur noch historische Notizen oder bewusst erhaltene Alias-Resolver stehen.
- VM-Re-Check nach Wiederverfuegbarkeit:
  `ssh ubuntu@10.0.0.240 'hostname; systemctl is-active sentinel-daemon sentinel-projection sentinel-gateway; curl -s localhost:8000/api/agents | python3 -c "import sys,json; data=json.load(sys.stdin); print(len(data)); print(data[0][\"current_room\"] if data else \"NO_AGENTS\")"'`
  Ergebnis: Host `sentinel-ubuntu-2404`, alle drei Dienste `active`, `26` Agents sichtbar.
- Dashboard-Regression lokal gruen:
  `bun test src/__tests__/events.test.ts src/routes/cockpit.test.ts`
  Ergebnis: `17` Tests gruen.
- Remote-Rust-Regressionen fuer den geaenderten Scope gruen:
  `cargo remote -c -- test -p sentinel-common --test acceptance` -> `2 passed`
  `cargo remote -c -- test -p sentinel-common --lib` -> `47 passed`
  `cargo remote -c -- test -p sentinel-daemon episode_producer::tests::` -> `13 passed`
  `cargo remote -c -- test -p sentinel-daemon test_fanout_topic_room_events` -> `1 passed`
  `cargo remote -c -- test -p sentinel-daemon build_gateway_request_formats_perception_for_gateway_compiler` -> `1 passed`
  `cargo remote -c -- test -p sentinel-daemon test_stress_cluster_detection` -> `1 passed`
  `cargo remote -c -- test -p sentinel-zenoh flatbuf` -> `20 passed`
  `cargo remote -c -- test -p sentinel-wasm --test acceptance` -> `10 passed`

## Task 5 evidence summary

- Neuer wiederholbarer Benchmark-Harness in `crates/sentinel-ecs` angelegt:
  `criterion` als Dev-Dependency plus `crates/sentinel-ecs/benches/room_phase2_bench.rs`.
- Compile-/Harness-Check gruen:
  `cargo remote -c -- bench -p sentinel-ecs --bench room_phase2_bench --no-run`
  Ergebnis: `Finished 'bench' profile [optimized]`.
- Frischer Voll-Lauf gruen:
  `cargo remote -c -- bench -p sentinel-ecs --bench room_phase2_bench -- --noplot --sample-size 20`
- Pflichtbenchmark `Route-BFS pro Move < 100us` klar eingehalten:
  `same_floor_2_hops = [2.5184 us 2.5293 us 2.5404 us]`
  `cross_floor_4_hops = [4.7902 us 4.8091 us 4.8259 us]`
  `upper_to_lower_wing = [4.7447 us 4.7709 us 4.7952 us]`
  `full_office_span = [5.1250 us 5.1348 us 5.1451 us]`
- Pflichtbenchmark `Encounter Detection pro Tick < 50us fuer 26 Agents` eingehalten:
  `room_phase2.encounter_detection_26_agents = [7.2499 us 7.2532 us 7.2572 us]`
- Zusaetzlicher dichter Stressfall dokumentiert:
  `room_phase2.encounter_detection_dense_26_agents = [202.62 us 205.70 us 209.46 us]`
  Dieser Messwert ist bewusst nicht das GitHub-Pflichtbudget, sondern Worst-Case-Evidence fuer einen unnatuerlich dichten Transit-Cluster.
- Pflichtbenchmark `Bio Tick-Duration darf nicht steigen < 1100ms` deutlich eingehalten:
  `room_phase2.bio_tick_26_agents = [155.71 us 156.77 us 157.65 us]`
  Der Repo-Harness liegt damit weit unter dem Budget; der verbleibende Live-Nachweis fuer `AC-17` folgt in Task 7 auf der VM.

## Task 3 repro steps

1. Stabilen Repro-Modus bestaetigen:
   `ssh ubuntu@10.0.0.240 "curl -s localhost:8081/control/config"`
   Erwartung: `rate_limit_rps=0`
2. Startzustand des Zielraums lesen:
   `ssh ubuntu@10.0.0.240 "curl -s localhost:8000/api/rooms | python3 -c 'import sys,json; rooms=json.load(sys.stdin); print(next(x for x in rooms if x[\"id\"]==\"buero-betriebsarzt\"))'"`
3. Gaia-Moves fuer OG-Agents ausloesen:
   `target_agent_id in {46,47,48,49,50}`, Thought: `Gehe jetzt bitte direkt ins buero-betriebsarzt.`
4. Move-Akzeptanz pruefen:
   Event-Store auf `operator_gaia_sent`, `agent_action_received`, `transit_started` fuer die Ziel-Agents abfragen.
5. Raumzustand pollen:
   `occupant_count` und `transit_count` fuer `buero-betriebsarzt` aus `/api/rooms` lesen.
6. Transit-vs-Occupancy pruefen:
   Agent-Zustaende in `/api/agents` mit Raumzustand kombinieren.
7. Fuer Einzel-Repro eines Direct-Moves:
   `AGENT-11 -> buero-it` oder denselben OG-Pfad erneut verwenden.

## AC matrix

| AC | Requirement | Primary trigger | Primary evidence | Expected signal |
|---|---|---|---|---|
| AC-1 | `Es ist eng hier` bei `capacity..capacity+2` | Agents gezielt in Raum bis Soft-Cap bewegen | `system` | Operator-/Daemon-Log oder Impulse zeigt `Es ist eng hier` |
| AC-2 | `Der Raum ist komplett voll` ueber `capacity+2` | Raum gezielt massiv ueberbelegen | `system` | Operator-/Daemon-Log oder Impulse zeigt `Der Raum ist komplett voll` |
| AC-3 | kein Hard-Block beim Eintritt | Agent per Operator/API in vollen Raum schicken | `system` | Transit/Move wird ausgefuehrt, Agent erreicht Ziel trotz Vollbelegung |
| AC-4 | stationaere Agents erhalten ebenfalls Kapazitaets-Hinweis | Raum nachtraeglich um stationaeren Agenten herum fuellen | `system` | bereits sitzender Agent erhaelt Capacity-Perception im Log/Impuls |
| AC-5 | `HallwayEncounterDetected` erzeugt P3 PendingEvent | zwei Transit-Agents in denselben Zwischen-Raum bringen | `system` | Event/Log weist Encounter PendingEvent nach |
| AC-6 | Encounter-Perception nennt Name und Rolle | denselben Encounter-Fall wie AC-5 ausloesen | `system` | Perception enthaelt `Du triffst [Name] ([Rolle]) im Flur` |
| AC-7 | Encounter nur im selben Zwischen-Raum | gleichzeitige Transit-Faelle auf verschiedenen Stockwerken und im Treppenhaus provozieren | `system` | kein Encounter fuer `flur-eg` vs `flur-og`, aber moeglich im `treppenhaus` |
| AC-8 | Transit pausiert bei Encounter | laufenden Encounter waehrend Transit beobachten | `system` | `remaining_ms`/Transit-Fortschritt stoppt oder Pause-Log erscheint |
| AC-9 | Transit resumed nach Encounter | nach Encounter-Chat/Timeout weiterbeobachten | `system` | Resume-Log/Event und spaetere Zielankunft |
| AC-10 | Transit-Dauer `20s/Hop`, clamp `15-120s` | 1-, 2-, 4- und 5-Hop-Moves ausloesen | `system` | Event-Store zeigt `duration_ms` im Zielbereich |
| AC-11 | Transit hat berechneten BFS-Pfad | Transit ueber bekannte adjacency-Routen ausloesen | `system` | Event/State/Log zeigt Route oder korrekte Zwischen-Raum-Sequenz |
| AC-12 | Agent hat jederzeit definierten Zwischen-Raum | Cross-floor-Transit ueber mehrere Ticks beobachten | `system` | API/Perception zeigt pro Tick einen konkreten Transit-Raum |
| AC-13 | Transit-Perception enthaelt Quelle, Ziel, Zwischen-Raum | waehrend Transit Impulse/Logs lesen | `system` | Text enthaelt `von`, `nach`, `durch` mit allen drei Angaben |
| AC-14 | stationaere Agents haben keinen Transit-Block | stationaeren Agenten in Ruhe beobachten | `system` | keine Transit-Perception bei `in_transit=false` |
| AC-15 | Heartbeat sinkt auf `2-3` Ticks bei aktivem Chat | 3+ Chat-Nachrichten in denselben Raum senden | `system` | Heartbeat-/Output-Logs zeigen kuerzere Intervalle |
| AC-16 | Heartbeat steigt wieder auf `10` Ticks | Chat stoppen und >30 Ticks warten | `system` | Heartbeat-Intervalle normalisieren sich auf Standard |
| AC-17 | Bio/Physics bleiben bei `1 Hz` | waehrend aktivem Chat Tick-Duration messen | `system` | Tick-/Metrikdaten bleiben um `1000ms`, unter `1100ms` |

## Commit references

- `7e4a72b` — Task 1: Baseline bestaetigen
- `6b30891` — Task 2: GitHub-AC-Matrix erstellen
- `27d536b` — Task 3: MO1-MO6 im laufenden System reproduzierbar machen
- `95be451` — Task 4: Room-ID-Drift und operatornahe Fixtures bereinigen
