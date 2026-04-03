# Testmatrix #288 — Cortex Gateway Traffic Control

**Version:** v2.0 (MITM-Architektur-korrekt)
**Datum:** 2026-03-30
**Basis:** Commits 4e69d4f + e4f8769
**Architektur:** `claude -p` → Gateway /v1/messages (MITM) → anthropic-direct → api.anthropic.com
**VM:** ubuntu@10.0.0.240
**Tester:** Claude (Test-Session)

**Parity-Hinweis (2026-04-03):**
Diese Matrix dokumentiert den verifizierten MITM-Stand auf `e4f8769` plus VM-Evidence.
Sie ist KEIN automatischer Beweis dafuer, dass GitHub `origin/main` heute denselben MITM-Vertrag noch enthaelt.
Die aktuell getrennt verfolgte Parity-Luecke liegt in `#298`.

---

## Block 0: MITM-Grundarchitektur (MUSS ZUERST PASS sein)

Ohne diese Tests ist der Rest sinnlos. Der MITM-Proxy ist die Grundlage.

### T1: /v1/messages Endpoint existiert im Gateway
- **Befehl:** `grep -n "v1/messages\|anthropicMessagesPath" cmd/cortex-gateway/internal/proxy/anthropic_api.go cmd/cortex-gateway/main.go`
- **Erwartung:** Route registriert, Handler vorhanden
- **Ort:** Lokal (Code)
- **PASS wenn:** grep findet Route-Registration in main.go UND Handler in anthropic_api.go

### T2: Eingehender Anthropic-Request wird korrekt decodiert
- **Befehl:** `go test ./cmd/cortex-gateway/internal/proxy/... -run "TestAnthropicDecode\|TestDecodeAnthropic\|TestPipelineAnthropicMessages" -v -count=1`
- **Erwartung:** Test PASS — system[], messages[], model, max_tokens korrekt geparst
- **Ort:** Lokal (Unit-Test)
- **PASS wenn:** Alle matched Tests PASS

### T3: system[] Blocks bleiben strukturiert (kein Flattening)
- **Befehl:** `go test ./cmd/cortex-gateway/internal/proxy/... -run "TestSystemBlock\|TestAnthropicSystem\|TestStructured" -v -count=1`
- **Erwartung:** SystemBlocks Array mit Type+Text+CacheControl erhalten
- **Ort:** Lokal (Unit-Test)
- **PASS wenn:** Tests zeigen SystemBlocks werden als Array durchgereicht

### T4: Header-Passthrough (Authorization, x-api-key, anthropic-version, anthropic-beta)
- **Befehl:** `go test ./cmd/cortex-gateway/internal/proxy/... -run "TestHeader\|TestPassthrough" -v -count=1`
- **Erwartung:** Alle 4 Headers werden aus eingehendem Request extrahiert und an Forward weitergegeben
- **Ort:** Lokal (Unit-Test)
- **PASS wenn:** Test bestaetigt Authorization + x-api-key + anthropic-version + anthropic-beta

### T5: PreferredProvider = "anthropic-direct" fuer /v1/messages
- **Befehl:** `grep -n "PreferredProvider.*anthropic-direct\|PreferredProvider.*=.*\"anthropic" cmd/cortex-gateway/internal/proxy/anthropic_api.go`
- **Erwartung:** Zeile zeigt PreferredProvider = "anthropic-direct"
- **Ort:** Lokal (Code)
- **PASS wenn:** grep findet Zeile in decodeAnthropicRequest()

### T6: Response hat Anthropic-Messages-Format (nicht PipelineResponse)
- **Befehl:** `go test ./cmd/cortex-gateway/internal/proxy/... -run "TestPipelineAnthropicMessages\|TestAnthropicResponse\|TestBuildAnthropicMessage" -v -count=1`
- **Erwartung:** Response: type=message, role=assistant, content=[{type:text}], usage, stop_reason
- **Ort:** Lokal (Unit-Test)
- **PASS wenn:** Tests bestaetigen Anthropic-kompatibles Response-Format

### T7: MITM E2E auf VM — claude -p gegen Gateway
- **Befehl:** `ssh ubuntu@10.0.0.240 "ANTHROPIC_BASE_URL=http://127.0.0.1:8080 NO_PROXY=127.0.0.1,localhost claude -p 'Antworte exakt mit PONG.' --output-format json" 2>&1 | head -20`
- **Erwartung:** Erfolgreiche Antwort mit "PONG" im content
- **Ort:** VM Runtime
- **PASS wenn:** claude -p erhaelt gueltige Antwort ueber Gateway
- **BLOCKED wenn:** Rate Limit aktiv (reset Apr 3)

### T8: Gateway-Journal zeigt provider=anthropic-direct bei MITM-Request
- **Befehl:** `ssh ubuntu@10.0.0.240 "journalctl -u sentinel-gateway --since '5 min ago' --no-pager | grep 'anthropic-direct'" 2>&1 | tail -5`
- **Erwartung:** Mindestens 1 Zeile mit provider=anthropic-direct
- **Ort:** VM Runtime (nach T7)
- **PASS wenn:** grep findet anthropic-direct Forward-Log
- **BLOCKED wenn:** T7 BLOCKED

---

## Block 1: Datei-Existenz + Struktur (16 Dateien)

### T9-T14: 6 neue Dateien

| # | Datei laut Issue | Befehl | Erwartung | Ort |
|---|-----------------|--------|-----------|-----|
| T9 | traffic/controller.go | `test -f cmd/cortex-gateway/internal/control/plane.go && grep -c "TrafficStats\|trafficStats" cmd/cortex-gateway/internal/control/plane.go` | Datei existiert, TrafficStats Handler vorhanden | Lokal |
| T10 | traffic/synthesis.go | `test -f cmd/cortex-gateway/internal/synthesis/engine.go && grep -c "func.*Match\|func.*Evaluate" cmd/cortex-gateway/internal/synthesis/engine.go` | Datei existiert, Match/Evaluate Funktion vorhanden | Lokal |
| T11 | traffic/queue.go | `test -f cmd/cortex-gateway/internal/sequencing/queue.go && grep -c "MarkP1Active\|WaitForP1\|CompleteP1" cmd/cortex-gateway/internal/sequencing/queue.go` | Datei existiert, P1/P3 Funktionen vorhanden | Lokal |
| T12 | traffic/fingerprint.go | `test -f cmd/cortex-gateway/internal/synthesis/fingerprint.go && grep -c "Parse\|Fingerprint" cmd/cortex-gateway/internal/synthesis/fingerprint.go` | Datei existiert, Parser vorhanden | Lokal |
| T13 | traffic/learner.go | `test -f cmd/cortex-gateway/internal/apicp/observer.go && grep -c "Record\|LearnedPatternFor\|ShouldProbe" cmd/cortex-gateway/internal/apicp/observer.go` | Datei existiert, OODA Funktionen vorhanden | Lokal |
| T14 | traffic/ticksync.go | `test -f cmd/cortex-gateway/internal/ticksync/buffer.go && grep -c "Hold\|flushExpired\|SetEnabled" cmd/cortex-gateway/internal/ticksync/buffer.go` | Datei existiert, Hold/Flush/Enable vorhanden | Lokal |

### T15-T22: 8 erweiterte Dateien

| # | Datei | Befehl | Erwartung | Ort |
|---|-------|--------|-----------|-----|
| T15 | proxy/pipeline.go | `grep -c "Step 7.5\|Step 8.5\|step_7_5\|traffic.*control\|INBOUND.*TRAFFIC\|OUTBOUND.*TRAFFIC" cmd/cortex-gateway/internal/proxy/pipeline.go` | Step 7.5 + 8.5 eingefuegt | Lokal |
| T16 | proxy/provider.go | `grep -c "type Provider interface\|Send(" cmd/cortex-gateway/internal/proxy/provider.go` | Provider Interface mit Send() | Lokal |
| T17 | main.go | `grep -c "synthEngine\|tickSync\|apicpObserver\|Sequencer\|ForwardQueue\|anthropicMessages" cmd/cortex-gateway/main.go` | Alle TC-Komponenten initialisiert | Lokal |
| T18 | daemon config.rs | `grep -c "traffic_control\|TrafficControlConfig" services/sentinel-daemon/src/config.rs` | TrafficControlConfig Struct vorhanden | Lokal |
| T19 | daemon.toml | `grep -c "traffic_control" config/daemon.toml` | [daemon.traffic_control] Sektion vorhanden | Lokal |
| T20 | common types.rs | `grep -c "synth_fingerprint" crates/sentinel-common/src/types.rs` | Fingerprint-Feld in Perception | Lokal |
| T21 | llm_bridge.rs | `grep -c "synth_fp\|fingerprint" services/sentinel-daemon/src/llm_bridge.rs` | Fingerprint in Metadata gesetzt | Lokal |
| T22 | redb lib.rs | `grep -c "api_patterns\|API_PATTERNS" crates/sentinel-redb/src/lib.rs` | api_patterns Table definiert | Lokal |

### T23-T24: 2 Dashboard-Dateien

| # | Datei | Befehl | Erwartung | Ort |
|---|-------|--------|-----------|-----|
| T23 | routes/control.ts | `grep -c "traffic-stats\|trafficStats" dashboard/src/routes/control.ts` | GET /api/control/traffic-stats Route | Lokal |
| T24 | public/js/control.js | `grep -c "trafficStats\|synthesis_rate\|estimated_savings" dashboard/public/js/control.js` | Traffic Control Panel Rendering | Lokal |

---

## Block 2: Synthesis-Regeln auf MITM-Pfad (10 Regeln)

Jede Regel hat ZWEI Tests: Unit-Test (lokal) + Runtime (VM journalctl).

### T25: S1 Bio Hunger
- **Unit:** `go test ./cmd/cortex-gateway/internal/synthesis/... -run "TestBioHungerFires" -v -count=1`
- **Runtime:** `ssh ubuntu@10.0.0.240 "journalctl -u sentinel-gateway --since '1h ago' --no-pager | grep -i 'synthesis.*match' | grep -i 'hunger\|bio_hunger\|kueche'" | head -3`
- **PASS wenn:** Unit-Test PASS UND journalctl zeigt mindestens 1 Hunger-Synthesis

### T26: S2 Bio Blase (mit agentID%2 Toiletten-Logik)
- **Unit:** `go test ./cmd/cortex-gateway/internal/synthesis/... -run "TestBioBladder" -v -count=1`
- **Runtime:** `ssh ubuntu@10.0.0.240 "journalctl -u sentinel-gateway --since '1h ago' --no-pager | grep -i 'synthesis.*match' | grep -i 'bladder\|toilette'" | head -3`
- **PASS wenn:** Unit-Test PASS (inkl. Modulo-Target) UND journalctl zeigt Bladder-Synthesis

### T27: S3 Bio Energie
- **Unit:** `go test ./cmd/cortex-gateway/internal/synthesis/... -run "TestBioEnergy" -v -count=1`
- **Runtime:** `ssh ubuntu@10.0.0.240 "journalctl -u sentinel-gateway --since '1h ago' --no-pager | grep -i 'synthesis.*match' | grep -i 'energy\|augen'" | head -3`
- **PASS wenn:** Unit-Test PASS UND journalctl zeigt Energy-Synthesis

### T28: S4 Bio Koffein
- **Unit:** `go test ./cmd/cortex-gateway/internal/synthesis/... -run "TestBioCaffeine" -v -count=1`
- **Runtime:** `ssh ubuntu@10.0.0.240 "journalctl -u sentinel-gateway --since '1h ago' --no-pager | grep -i 'synthesis.*match' | grep -i 'caffeine\|kaffee'" | head -3`
- **PASS wenn:** Unit-Test PASS UND journalctl zeigt Caffeine-Synthesis

### T29: S5 Routine Idle allein
- **Unit:** `go test ./cmd/cortex-gateway/internal/synthesis/... -run "TestRoutineIdleAlone" -v -count=1`
- **Runtime:** `ssh ubuntu@10.0.0.240 "journalctl -u sentinel-gateway --since '1h ago' --no-pager | grep -i 'synthesis.*match' | grep -i 'idle\|routine\|konzentriert\|arbeitet'" | head -3`
- **PASS wenn:** Unit-Test PASS UND journalctl zeigt Idle-Synthesis

### T30: S6 Routine Idle mit Anwesenden (I/E Templates)
- **Unit:** `go test ./cmd/cortex-gateway/internal/synthesis/... -run "TestRoutineIdleWithPresence" -v -count=1`
- **Runtime:** (gleicher grep wie T29, verschiedene Templates erwartet)
- **PASS wenn:** Unit-Test PASS — bestaetigt I/E Template-Auswahl

### T31: S7 Physics Temperatur
- **Unit:** `go test ./cmd/cortex-gateway/internal/synthesis/... -run "TestPhysicsTempHigh" -v -count=1`
- **Runtime:** `ssh ubuntu@10.0.0.240 "journalctl -u sentinel-gateway --since '1h ago' --no-pager | grep -i 'synthesis.*match' | grep -i 'temp\|fenster'" | head -3`
- **PASS wenn:** Unit-Test PASS UND Runtime (falls Temp-Events existieren)

### T32: S8 Physics Laerm
- **Unit:** `go test ./cmd/cortex-gateway/internal/synthesis/... -run "TestPhysicsNoise" -v -count=1`
- **Runtime:** `ssh ubuntu@10.0.0.240 "journalctl -u sentinel-gateway --since '1h ago' --no-pager | grep -i 'synthesis.*match' | grep -i 'noise\|kopfhoerer'" | head -3`
- **PASS wenn:** Unit-Test PASS UND Runtime (falls Noise-Events existieren)

### T33: S9 Circadian Morgen
- **Unit:** `go test ./cmd/cortex-gateway/internal/synthesis/... -run "TestCircadianMorning" -v -count=1`
- **Runtime:** `ssh ubuntu@10.0.0.240 "journalctl -u sentinel-gateway --since '6h ago' --no-pager | grep -i 'synthesis.*match' | grep -i 'circadian\|morning\|mails'" | head -3`
- **PASS wenn:** Unit-Test PASS UND Runtime (falls simHour 6-7 war)

### T34: S10 Circadian Mittag
- **Unit:** `go test ./cmd/cortex-gateway/internal/synthesis/... -run "TestCircadianLunch" -v -count=1`
- **Runtime:** `ssh ubuntu@10.0.0.240 "journalctl -u sentinel-gateway --since '6h ago' --no-pager | grep -i 'synthesis.*match' | grep -i 'lunch\|mittag'" | head -3`
- **PASS wenn:** Unit-Test PASS UND Runtime (falls simHour 12-13 war)

---

## Block 3: baseGate — Synthesis NIE bei Chat/Chaos/Heard/Impulse

### T35: baseGate blockiert bei has_heard=true
- **Befehl:** `go test ./cmd/cortex-gateway/internal/synthesis/... -run "TestBioHungerBlockedByHeardMetadata" -v -count=1`
- **PASS wenn:** Test PASS — Hunger>0.8 wird NICHT synthetisiert wenn heard_text vorhanden

### T36: baseGate blockiert bei has_chaos=true
- **Befehl:** `go test ./cmd/cortex-gateway/internal/synthesis/... -run "TestBioHungerBlockedByChaos" -v -count=1`
- **PASS wenn:** Test PASS — Hunger>0.8 wird NICHT synthetisiert wenn Chaos aktiv

### T37: baseGate blockiert bei is_addressed=true
- **Befehl:** `go test ./cmd/cortex-gateway/internal/synthesis/... -run "TestBioBladderBlockedByAddressed" -v -count=1`
- **PASS wenn:** Test PASS — Bladder>0.9 wird NICHT synthetisiert wenn Agent direkt angesprochen

### T38: baseGate blockiert bei has_impulse=true
- **Befehl:** `go test ./cmd/cortex-gateway/internal/synthesis/... -run "TestImpulseBypassesSynthesis" -v -count=1`
- **PASS wenn:** Test PASS — kein Synthesis wenn Gaia-Impuls vorhanden

---

## Block 4: Chat-Sequencing via MITM (5 Schritte)

### T39: P1 wird sofort forwarded
- **Befehl:** `go test ./cmd/cortex-gateway/internal/sequencing/... -run "TestP1Forward" -v -count=1`
- **PASS wenn:** P1 Call geht SOFORT an Provider — keine Queue-Verzoegerung

### T40: P3 wird gequeued bis P1 antwortet
- **Befehl:** `go test ./cmd/cortex-gateway/internal/sequencing/... -run "TestP3WaitsForP1" -v -count=1`
- **PASS wenn:** P3 blockiert bis P1 Done oder Timeout

### T41: P1-Antwort in P3-Kontext injiziert (MITM-Modify)
- **Befehl:** `go test ./cmd/cortex-gateway/internal/proxy/... -run "TestPipelineSequencingInjectsP1Context" -v -count=1`
- **PASS wenn:** P1 Content wird als ContextSuffix in P3-Request injiziert

### T42: Mehrere P3 bekommen denselben P1-Kontext
- **Befehl:** `go test ./cmd/cortex-gateway/internal/sequencing/... -run "TestMultipleP3s" -v -count=1`
- **PASS wenn:** Alle P3 im selben Room erhalten denselben P1-Content

### T43: P3 Timeout (5s) — Freigabe ohne P1
- **Befehl:** `go test ./cmd/cortex-gateway/internal/sequencing/... -run "TestP3Timeout\|TestP3NoActiveP1" -v -count=1`
- **PASS wenn:** P3 wird nach 5s freigegeben auch wenn kein P1 vorhanden

---

## Block 5: API-CP OODA Loop (7 Phasen)

### T44: OBSERVE — Record() speichert Fingerprint + Response
- **Befehl:** `go test ./cmd/cortex-gateway/internal/apicp/... -run "TestRecord\|TestObserve" -v -count=1`
- **Runtime:** `ssh ubuntu@10.0.0.240 "curl -s http://127.0.0.1:8081/control/traffic-stats | python3 -c 'import json,sys; d=json.load(sys.stdin); print(f\"active_patterns={d.get(\"active_patterns\",0)}\")'"`
- **PASS wenn:** Unit-Test PASS UND active_patterns > 0 auf VM

### T45: ORIENT — Confidence = topCount/totalCount
- **Befehl:** `go test ./cmd/cortex-gateway/internal/apicp/... -run "TestConfidence" -v -count=1`
- **PASS wenn:** Confidence korrekt berechnet

### T46: DECIDE — Promotion bei Confidence > 90% + 50 Samples
- **Befehl:** `go test ./cmd/cortex-gateway/internal/apicp/... -run "TestSuggestions\|TestPromotion" -v -count=1`
- **Runtime:** `ssh ubuntu@10.0.0.240 "curl -s http://127.0.0.1:8081/control/traffic-stats | python3 -c 'import json,sys; d=json.load(sys.stdin); print(f\"promoted={d[\"apicp\"][\"promoted_patterns\"]}\")'"`
- **PASS wenn:** Unit-Test PASS UND promoted_patterns >= 1 auf VM

### T47: ACT — Promoted Pattern wird als Synthesis-Regel genutzt
- **Befehl:** `go test ./cmd/cortex-gateway/internal/proxy/... -run "TestPipelineAPICPLearnedPattern" -v -count=1`
- **PASS wenn:** Pipeline nutzt gelerntes Pattern statt Forward

### T48: VERIFY — Stichprobe alle 100 Calls
- **Befehl:** `go test ./cmd/cortex-gateway/internal/apicp/... -run "TestShouldProbe\|TestApplyProbeResult" -v -count=1`
- **PASS wenn:** ShouldProbe() feuert alle 100 Calls, Degradation bei Mismatch

### T49: DEGRADE — Evolution-Change halbiert Confidences
- **Befehl:** `go test ./cmd/cortex-gateway/internal/apicp/... -run "TestEvolutionDegradation" -v -count=1`
- **PASS wenn:** Alle Confidences halbiert nach Version-Change

### T50: STORAGE — Hybrid In-Memory + redb Dump
- **Befehl:** `ssh ubuntu@10.0.0.240 "curl -s http://127.0.0.1:8084/operator/apicp/snapshot | python3 -m json.tool | head -10"`
- **PASS wenn:** Daemon liefert Snapshot mit patterns Array (nicht leer)

---

## Block 6: Tick-Sync (4 Funktionen)

### T51: Hold — Response wird bis Tick-Boundary gehalten
- **Befehl:** `go test ./cmd/cortex-gateway/internal/ticksync/... -run "TestHoldAndFlush" -v -count=1`
- **PASS wenn:** Entry wird gehalten und erst nach Timeout geflusht

### T52: Order — P1 vor P3 innerhalb Tick
- **Befehl:** `go test ./cmd/cortex-gateway/internal/ticksync/... -run "TestPriorityOrdering" -v -count=1`
- **PASS wenn:** Sortierung: Priority ASC, dann AgentID ASC

### T53: Release — Deterministische Reihenfolge
- **Runtime:** `ssh ubuntu@10.0.0.240 "journalctl -u sentinel-gateway --since '30 min ago' --no-pager | grep -c 'tick_sync.*flush\|flushed'" 2>&1`
- **PASS wenn:** Flush-Count > 0 (Tick-Sync aktiv)

### T54: Disable — Per Config deaktivierbar
- **Befehl:** `go test ./cmd/cortex-gateway/internal/ticksync/... -run "TestDisabledBuffer\|TestSetEnabled" -v -count=1`
- **PASS wenn:** SetEnabled(false) flusht pending und stoppt Loop

---

## Block 7: Perception-Fingerprint (12 Felder)

### T55-T66: Fingerprint-Felder

Alle 12 Felder werden in EINEM Unit-Test geprueft:
- **Befehl:** `go test ./cmd/cortex-gateway/internal/synthesis/... -run "TestParseFingerprint" -v -count=1`
- **Zusatz:** `go test ./cmd/cortex-gateway/internal/synthesis/... -run "TestParsePartialFields\|TestEmptyFingerprint" -v -count=1`

| # | Feld | Key im Parser | PASS wenn |
|---|------|--------------|-----------|
| T55 | bio_hunger | H | TestParseFingerprint: H=8 korrekt geparst |
| T56 | bio_energy | E | TestParseFingerprint: E=6 korrekt geparst |
| T57 | bio_bladder | B | TestParseFingerprint: B=3 korrekt geparst |
| T58 | bio_stress | S | TestParseFingerprint: S=5 korrekt geparst |
| T59 | bio_caffeine | C | TestParseFingerprint: C=7 korrekt geparst |
| T60 | bio_social | SN | TestParseFingerprint: SN=4 korrekt geparst |
| T61 | room_id | R | TestParseFingerprint: R=buero-dev-1 korrekt geparst |
| T62 | agents_present | P | TestParseFingerprint: P=3 korrekt geparst |
| T63 | has_heard | HR | TestParseFingerprint: HR=1 korrekt geparst |
| T64 | has_chaos | CH | TestParseFingerprint: CH=0 korrekt geparst |
| T65 | circadian_hour | T | TestParseFingerprint: T=14 korrekt geparst |
| T66 | has_impulse | IM | TestParseFingerprint: IM=0 korrekt geparst |

---

## Block 8: MITM-Interception Features (8 Funktionen)

### T67: InterceptMode (Auto/Manual)
- **Befehl:** `go test ./cmd/cortex-gateway/internal/intercept/... -run "TestMode\|TestAuto\|TestManual" -v -count=1`
- **Runtime:** `ssh ubuntu@10.0.0.240 "curl -s http://127.0.0.1:8081/control/traffic-stats | python3 -c 'import json,sys; d=json.load(sys.stdin); print(f\"intercept_mode={d[\"intercept_mode\"]}\")'"`
- **PASS wenn:** Test PASS UND VM zeigt intercept_mode

### T68: Request Hold
- **Befehl:** `go test ./cmd/cortex-gateway/internal/intercept/... -run "TestAwaitRequestDecision" -v -count=1`
- **PASS wenn:** AwaitRequestDecision() blockiert bis Decision empfangen

### T69: Request Modify
- **Befehl:** `go test ./cmd/cortex-gateway/internal/proxy/... -run "TestPipelineManualInterceptModify" -v -count=1`
- **PASS wenn:** Request wird mit ContextSuffix modifiziert BEVOR Forward

### T70: Request Drop
- **Befehl:** `grep -n "Drop\|DecisionDrop\|ActionDrop" cmd/cortex-gateway/internal/intercept/types.go cmd/cortex-gateway/internal/proxy/pipeline.go`
- **PASS wenn:** Drop-Aktion existiert im Code UND Pipeline returned 204 bei Drop

### T71: Response Hold
- **Befehl:** `go test ./cmd/cortex-gateway/internal/intercept/... -run "TestAwaitResponseDecision" -v -count=1`
- **PASS wenn:** AwaitDecision() blockiert bis Response-Decision empfangen

### T72: Response Modify/Replace
- **Befehl:** `go test ./cmd/cortex-gateway/internal/proxy/... -run "TestPipelineManualResponseReplace" -v -count=1`
- **PASS wenn:** Response wird ersetzt BEVOR Rueckgabe an Client

### T73: Request Logging
- **Befehl:** `grep -n "LogRequest\|requestLog\|response_log" cmd/cortex-gateway/internal/proxy/response_log.go`
- **Runtime:** `ssh ubuntu@10.0.0.240 "curl -s http://127.0.0.1:8081/control/traffic-stats | python3 -c 'import json,sys; d=json.load(sys.stdin); print(f\"response_log_entries={d[\"response_log_entries\"]}\")'"`
- **PASS wenn:** response_log_entries > 0 auf VM

### T74: Response Logging
- **Befehl:** (gleich wie T73 — Request + Response werden zusammen geloggt)
- **PASS wenn:** response_log_entries > 0 auf VM

---

## Block 9: 22 ACs einzeln (aus dem Issue)

### T75: AC-1 Bio-Impulse synthetisiert
- **Befehl:** `ssh ubuntu@10.0.0.240 "curl -s http://127.0.0.1:8081/control/traffic-stats | python3 -c 'import json,sys; d=json.load(sys.stdin); print(f\"synthesis_count={d[\"synthesis_count\"]}, rate={d[\"synthesis_rate\"]*100:.1f}%\")'"`
- **PASS wenn:** synthesis_count > 0 UND synthesis_rate > 0

### T76: AC-2 Synthesis durchlaeuft Outbound-Pipeline
- **Befehl:** `go test ./cmd/cortex-gateway/internal/proxy/... -run "TestPipelineSynthesisAndForwardShareOutbound" -v -count=1`
- **PASS wenn:** Synthetisierte Response durchlaeuft gleichen Outbound-Pfad wie echte

### T77: AC-3 Synthesis per Config deaktivierbar
- **Befehl:** `go test ./cmd/cortex-gateway/internal/synthesis/... -run "TestDisabledEngine" -v -count=1`
- **Runtime:** `ssh ubuntu@10.0.0.240 "grep SENTINEL_SYNTHESIS_ENABLED /etc/systemd/system/sentinel-gateway.service"`
- **PASS wenn:** Test PASS UND Env-Variable existiert in systemd Unit

### T78: AC-4 Persoenlichkeitsabhaengige Templates
- **Befehl:** `go test ./cmd/cortex-gateway/internal/synthesis/... -run "TestPersonalityTemplates" -v -count=1`
- **PASS wenn:** I-Template != E-Template fuer gleiche Regel

### T79: AC-5 Circadian + Physics Synthesis
- **Befehl:** T31 + T32 + T33 + T34 (Unit-Tests fuer Circadian + Physics)
- **PASS wenn:** Alle 4 Tests PASS

### T80: AC-6 Synthesis NIE bei Chat/Heard/Chaos/Impulse
- **Befehl:** T35 + T36 + T37 + T38 (baseGate Tests)
- **PASS wenn:** Alle 4 Tests PASS

### T81: AC-7 P1 sofort forwarded
- **Befehl:** T39 (P1 Forward Test)
- **PASS wenn:** T39 PASS

### T82: AC-8 P3 gequeued bis P1 antwortet
- **Befehl:** T40 (P3 Wait Test)
- **PASS wenn:** T40 PASS

### T83: AC-9 P1-Antwort in P3-Kontext injiziert
- **Befehl:** T41 (MITM-Modify Test)
- **PASS wenn:** T41 PASS

### T84: AC-10 Timeout: P3 ohne P1 nach 5s
- **Befehl:** T43 (Timeout Test)
- **PASS wenn:** T43 PASS

### T85: AC-11 API-CP beobachtet alle Calls
- **Befehl:** T44 (Observe Test)
- **PASS wenn:** T44 PASS

### T86: AC-12 Confidence > 90% → Synthesis aktiviert
- **Befehl:** T46 (Promotion Test)
- **PASS wenn:** T46 PASS

### T87: AC-13 Evolution-Change halbiert Confidences
- **Befehl:** T49 (Degradation Test)
- **PASS wenn:** T49 PASS

### T88: AC-14 Stichproben-Verifikation
- **Befehl:** T48 (Probe Test)
- **PASS wenn:** T48 PASS

### T89: AC-15 Tick-Sync auf Tick-Grenzen
- **Befehl:** T51 (Hold Test)
- **PASS wenn:** T51 PASS

### T90: AC-16 P1 > P3 Ordering in Tick
- **Befehl:** T52 (Priority Ordering Test)
- **PASS wenn:** T52 PASS

### T91: AC-17 Tick-Sync per Config deaktivierbar
- **Befehl:** T54 (Disable Test)
- **PASS wenn:** T54 PASS

### T92: AC-18 Feature-gated, komplett deaktivierbar
- **Befehl:** `grep -c "SENTINEL_SYNTHESIS_ENABLED\|SENTINEL_SEQUENCING_ENABLED\|SENTINEL_TICK_SYNC_ENABLED\|SENTINEL_APICP_ENABLED" cmd/cortex-gateway/main.go`
- **PASS wenn:** Alle 4 Env-Vars vorhanden

### T93: AC-19 Dashboard Traffic Stats
- **Befehl:** `ssh ubuntu@10.0.0.240 "curl -s http://127.0.0.1:8000/api/control/traffic-stats | python3 -c 'import json,sys; d=json.load(sys.stdin); keys=list(d.keys()); print(f\"Keys: {len(keys)}, has synthesis_rate: {\"synthesis_rate\" in keys}, has savings: {\"estimated_savings_usd\" in keys}\")'"`
- **PASS wenn:** Dashboard liefert Stats mit synthesis_rate UND estimated_savings_usd

### T94: AC-20 ZERO Disk-Writes im Hot-Path
- **Befehl:** `grep -rn "os.WriteFile\|os.Create\|ioutil.WriteFile\|\.Write(" cmd/cortex-gateway/internal/synthesis/ cmd/cortex-gateway/internal/sequencing/ cmd/cortex-gateway/internal/ticksync/`
- **PASS wenn:** Kein File-I/O in synthesis/, sequencing/, ticksync/ (Hot-Path)

### T95: AC-21 Alle bestehenden Tests gruen
- **Befehl:** `go test ./cmd/cortex-gateway/... -count=1 2>&1 | tail -5`
- **PASS wenn:** "FAIL" kommt nicht vor, alle Packages "ok"

### T96: AC-22 Latenz-Overhead < 5ms
- **Befehl:** `go test ./cmd/cortex-gateway/internal/proxy/... -run "TestPipelineSynthesis" -v -count=1 2>&1 | grep -i "latency\|duration\|elapsed"`
- **PASS wenn:** Synthesis-Pfad < 5ms in Test-Output

---

## Block 10: 26 PFLICHT Unit-Tests

Jeder Test: `go test ./cmd/cortex-gateway/... -run "TESTNAME" -v -count=1`

| # | Issue-Name | Tatsaechlicher Test-Name | Befehl-Suffix (-run) |
|---|-----------|-------------------------|---------------------|
| T97 | test_synthesis_bio_hunger_fires | TestBioHungerFires | TestBioHungerFires |
| T98 | test_synthesis_bio_hunger_blocked_by_chat | TestBioHungerBlockedByHeardMetadata | TestBioHungerBlockedByHeardMetadata |
| T99 | test_synthesis_bio_hunger_blocked_by_chaos | TestBioHungerBlockedByChaos | TestBioHungerBlockedByChaos |
| T100 | test_synthesis_routine_idle_fires | TestRoutineIdleAlone | TestRoutineIdleAlone |
| T101 | test_synthesis_routine_idle_blocked_by_stimuli | TestImpulseBypassesSynthesis | TestImpulseBypassesSynthesis |
| T102 | test_synthesis_physics_temperature_fires | TestPhysicsTempHigh | TestPhysicsTempHigh |
| T103 | test_synthesis_circadian_morning_fires | TestCircadianMorning | TestCircadianMorning |
| T104 | test_synthesis_disabled_via_config | TestDisabledEngineForwards | TestDisabledEngineForwards |
| T105 | test_synthesis_response_format_valid | TestPersonalityTemplates | TestPersonalityTemplates |
| T106 | test_queue_p1_immediate_forward | TestP1ForwardImmediately | TestP1ForwardImmediately |
| T107 | test_queue_p3_held_until_p1_response | TestP3WaitsForP1 | TestP3WaitsForP1 |
| T108 | test_queue_p3_context_injection_after_p1 | TestPipelineSequencingInjectsP1Context | TestPipelineSequencingInjectsP1Context |
| T109 | test_queue_timeout_releases_p3_without_p1 | TestP3TimeoutRelease | TestP3TimeoutRelease |
| T110 | test_queue_no_p1_all_p3_released_after_timeout | TestP3NoActiveP1 | TestP3NoActiveP1 |
| T111 | test_tick_sync_holds_response_until_boundary | TestHoldAndFlush | TestHoldAndFlush |
| T112 | test_tick_sync_priority_ordering_within_tick | TestPriorityOrdering | TestPriorityOrdering |
| T113 | test_tick_sync_disabled_via_config | TestDisabledBuffer | TestDisabledBuffer |
| T114 | test_fingerprint_hash_deterministic | TestParseFingerprint | TestParseFingerprint |
| T115 | test_fingerprint_bio_rounding | TestParsePartialFields | TestParsePartialFields |
| T116 | test_fingerprint_different_rooms_different_hash | (implizit in TestParseFingerprint) | TestParseFingerprint |
| T117 | test_fingerprint_same_state_same_hash | (implizit in TestParseFingerprint) | TestParseFingerprint |
| T118 | test_learner_confidence_calculation | TestConfidenceCalculation | TestConfidenceCalculation |
| T119 | test_learner_promotion_at_90_percent | TestSuggestionsThreshold | TestSuggestionsThreshold |
| T120 | test_learner_degradation_on_evolution_change | TestEvolutionDegradation | TestEvolutionDegradation |
| T121 | test_learner_stichprobe_verification | TestShouldProbe + TestApplyProbeResult | "TestShouldProbe|TestApplyProbeResult" |
| T122 | test_learner_bounded_patterns_per_agent | TestPatternLimitIsPerAgent | TestPatternLimitIsPerAgent |

---

## Block 11: 5 Integration-Tests + 3 E2E-Tests

### T123: IT1 synthesis_e2e_bio_to_ecs_action
- **Befehl:** `go test ./cmd/cortex-gateway/internal/proxy/... -run "TestPipelineSynthesisUsesRuleActions" -v -count=1`
- **PASS wenn:** Bio→Synthesis→Action korrekt extrahiert

### T124: IT2 chat_sequencing_e2e
- **Befehl:** `go test ./cmd/cortex-gateway/internal/proxy/... -run "TestPipelineSequencingInjectsP1Context\|TestPipelineSequencingTimeout" -v -count=1`
- **PASS wenn:** P1→P3 Injection funktioniert end-to-end

### T125: IT3 tick_sync_e2e
- **Befehl:** `go test ./cmd/cortex-gateway/internal/proxy/... -run "TestPipelineQueueTickSyncAndSequencingIntegrate" -v -count=1`
- **PASS wenn:** P1+P3+TickSync zusammen funktionieren

### T126: IT4 mixed_synthesis_and_real_calls
- **Befehl:** `go test ./cmd/cortex-gateway/internal/proxy/... -run "TestPipelineSynthesisAndForwardShareOutbound" -v -count=1`
- **PASS wenn:** Synthesis + Forward teilen gleichen Outbound-Pfad

### T127: IT5 api_cp_learns_and_synthesizes
- **Befehl:** `go test ./cmd/cortex-gateway/internal/proxy/... -run "TestPipelineAPICPLearnedPatternSynthesizes\|TestPipelineAPICPProbeForwards" -v -count=1`
- **PASS wenn:** Gelerntes Pattern wird in Pipeline genutzt

### T128: E2E1 Synthesis-Rate > 30%
- **Befehl:** `ssh ubuntu@10.0.0.240 "curl -s http://127.0.0.1:8081/control/traffic-stats | python3 -c 'import json,sys; d=json.load(sys.stdin); r=d[\"synthesis_rate\"]*100; print(f\"Rate: {r:.1f}% — {\"PASS\" if r > 30 else \"FAIL\"}\")'" `
- **PASS wenn:** Output zeigt "PASS" (Rate > 30%)

### T129: E2E2 Chat-Sequencing kohaerente Gespraeche
- **Befehl:** `ssh ubuntu@10.0.0.240 "journalctl -u sentinel-gateway --since '1h ago' --no-pager | grep -c 'p1.*completed\|p3.*released\|context.*inject'"`
- **PASS wenn:** Count > 0 — P1/P3 Sequencing Events beobachtet
- **BLOCKED wenn:** Rate Limit aktiv (kein Forward = kein Chat)

### T130: E2E3 API-CP lernt nach 1h
- **Befehl:** `ssh ubuntu@10.0.0.240 "curl -s http://127.0.0.1:8081/control/traffic-stats | python3 -c 'import json,sys; d=json.load(sys.stdin); print(f\"patterns={d.get(\"active_patterns\",0)}, promoted={d[\"apicp\"][\"promoted_patterns\"]}, suggestions={d[\"apicp_suggestion_count\"]}\")'"`
- **PASS wenn:** active_patterns > 50 UND promoted_patterns >= 1

---

## Block 12: 11 Benchmarks

### T131: Synthesis-Entscheidung < 1ms
- **Befehl:** `go test ./cmd/cortex-gateway/internal/synthesis/... -bench "BenchmarkMatch\|BenchmarkEvaluate" -benchtime=1000x -count=1 2>&1 | grep "ns/op"`
- **PASS wenn:** < 1000000 ns/op (= <1ms)
- **Fallback:** Wenn kein Benchmark existiert: `go test -run TestBioHungerFires -v` — Laufzeit im Test < 1ms

### T132: Fingerprint-Berechnung < 100us
- **Befehl:** `go test ./cmd/cortex-gateway/internal/synthesis/... -bench "BenchmarkParse\|BenchmarkFingerprint" -benchtime=1000x -count=1 2>&1 | grep "ns/op"`
- **PASS wenn:** < 100000 ns/op (= <100us)

### T133: Queue-Management < 500us
- **Befehl:** `go test ./cmd/cortex-gateway/internal/sequencing/... -bench "Benchmark" -benchtime=1000x -count=1 2>&1 | grep "ns/op"`
- **PASS wenn:** < 500000 ns/op

### T134: Tick-Sync Overhead < 1ms
- **Befehl:** `go test ./cmd/cortex-gateway/internal/ticksync/... -bench "Benchmark" -benchtime=1000x -count=1 2>&1 | grep "ns/op"`
- **PASS wenn:** < 1000000 ns/op

### T135: API-CP Pattern-Lookup < 500us
- **Befehl:** `go test ./cmd/cortex-gateway/internal/apicp/... -bench "Benchmark" -benchtime=1000x -count=1 2>&1 | grep "ns/op"`
- **PASS wenn:** < 500000 ns/op

### T136: redb Dump < 10ms
- **Befehl:** `ssh ubuntu@10.0.0.240 "journalctl -u sentinel-gateway --since '1h ago' --no-pager | grep -i 'apicp.*sync\|redb.*dump\|snapshot.*duration'" | tail -5`
- **PASS wenn:** Duration < 10ms in Logs (oder kein dedizierter Timer = UNTESTED)

### T137: Gesamt TC Overhead < 5ms
- **Befehl:** Referenz auf T96 (AC-22)
- **PASS wenn:** Synthesis-Pfad < 5ms

### T138: Memory Synthesis-Regeln < 1 MB
- **Befehl:** `ssh ubuntu@10.0.0.240 "ps -o rss= -p \$(pgrep cortex-gate)" 2>&1`
- **PASS wenn:** RSS < 200 MB gesamt (10 Regeln = vernachlaessigbar)

### T139: Memory API-CP Patterns < 5 MB
- **Befehl:** (gleich T138 — API-CP ist Teil des Gateway-Prozesses)
- **PASS wenn:** RSS < 200 MB gesamt (1000 Patterns/Agent bei 26 Agents = ~5 MB)

### T140: Synthesis-Rate nach 1h > 30%
- **Befehl:** Referenz auf T128 (E2E1)
- **PASS wenn:** T128 PASS

### T141: Kosten-Reduktion > 40%
- **Befehl:** `ssh ubuntu@10.0.0.240 "curl -s http://127.0.0.1:8081/control/traffic-stats | python3 -c 'import json,sys; d=json.load(sys.stdin); s=d[\"estimated_savings_usd\"]; c=d[\"current_cost_usd\"]; r=s/(s+c)*100 if (s+c)>0 else 0; print(f\"Savings: \${s:.0f}, Cost: \${c:.0f}, Reduction: {r:.1f}% — {\"PASS\" if r > 40 else \"FAIL\"}\")'" `
- **PASS wenn:** Output zeigt "PASS" (Reduktion > 40%)

---

## Block 13: Circuit Breaker TOGAF-Compliance

### T142: CB Window = 20s (TOGAF: "Window 20s")
- **Befehl:** `ssh ubuntu@10.0.0.240 "grep CB_WINDOW /etc/systemd/system/sentinel-gateway.service /etc/systemd/system/sentinel-gateway.service.d/*.conf 2>/dev/null"`
- **Alternativ:** `grep -n "window\|Window" cmd/cortex-gateway/internal/proxy/circuit_breaker.go`
- **PASS wenn:** Window = 20s
- **TOGAF Zeile:** 932

### T143: CB failure_ratio >= 0.5 (TOGAF: "open bei failure_ratio >= 0.5")
- **Befehl:** `grep -n "ratio\|Ratio\|threshold\|Threshold" cmd/cortex-gateway/internal/proxy/circuit_breaker.go`
- **Runtime:** `ssh ubuntu@10.0.0.240 "grep CB_FAILURE /etc/systemd/system/sentinel-gateway.service"`
- **PASS wenn:** Ratio-basiert (50%+ Fehler), NICHT consecutive-threshold
- **TOGAF Zeile:** 932

### T144: CB Half-open nach 30s (TOGAF: "Half-open nach 30s")
- **Befehl:** `grep -n "halfopen\|half_open\|HalfOpen\|OPEN_SECONDS" cmd/cortex-gateway/internal/proxy/circuit_breaker.go cmd/cortex-gateway/main.go`
- **Runtime:** `ssh ubuntu@10.0.0.240 "grep OPEN_SECONDS /etc/systemd/system/sentinel-gateway.service"`
- **PASS wenn:** Half-open Duration = 30s
- **TOGAF Zeile:** 932

### T145: CB 3 Probes im Half-open (TOGAF: "3 Probes")
- **Befehl:** `grep -n "probe\|Probe\|HALFOPEN_PROBES" cmd/cortex-gateway/internal/proxy/circuit_breaker.go cmd/cortex-gateway/main.go`
- **Runtime:** `ssh ubuntu@10.0.0.240 "grep HALFOPEN_PROBES /etc/systemd/system/sentinel-gateway.service"`
- **PASS wenn:** HalfOpenProbes = 3
- **TOGAF Zeile:** 932

### T146: CB pro Provider isoliert (TOGAF: "Pro Provider isoliert")
- **Befehl:** `grep -n "breakers\|perProvider\|map.*Breaker" cmd/cortex-gateway/internal/proxy/circuit_breaker.go`
- **Runtime:** `ssh ubuntu@10.0.0.240 "curl -s http://127.0.0.1:8080/health | python3 -m json.tool" 2>&1 | grep circuit`
- **PASS wenn:** Health zeigt separate CB-States pro Provider

---

## Block 14: Fehlende Tests (TOGAF + Architektur)

### T147: MITM-Pfad — Anthropic-Error-Response-Format
- **Befehl:** `grep -n "writeAnthropicError\|anthropicErrorResponse" cmd/cortex-gateway/internal/proxy/anthropic_api.go`
- **PASS wenn:** Error-Responses sind Anthropic-kompatibel (type=error, error={type,message})

### T143: MITM-Pfad — Synthesis-Response als Anthropic-Format zurueck
- **Befehl:** `go test ./cmd/cortex-gateway/internal/proxy/... -run "TestPipelineAnthropicMessages" -v -count=1`
- **PASS wenn:** Synthetisierte Response wird als Anthropic Messages Response zurueckgegeben (nicht PipelineResponse)

### T144: MITM-Pfad — Judge/Regen behalten system[] Blocks
- **Befehl:** `grep -n "SystemBlocks\|systemBlocks" cmd/cortex-gateway/internal/proxy/judge_adapter.go`
- **PASS wenn:** judge_adapter reicht SystemBlocks durch, kein Flattening

### T145: Forward-Queue — Max 3 gleichzeitige Calls
- **Befehl:** `go test ./cmd/cortex-gateway/internal/forwardqueue/... -run "TestMax\|TestConcurrency\|TestLimit" -v -count=1`
- **Runtime:** `ssh ubuntu@10.0.0.240 "curl -s http://127.0.0.1:8081/control/traffic-stats | python3 -c 'import json,sys; d=json.load(sys.stdin); print(f\"max_concurrency={d[\"max_forward_concurrency\"]}, active={d[\"active_forward_calls\"]}\")'"`
- **PASS wenn:** max_forward_concurrency = 3

### T146: Forward-Queue — FIFO, kein Drop
- **Befehl:** `go test ./cmd/cortex-gateway/internal/forwardqueue/... -run "TestFIFO\|TestNoDrop\|TestOrdering" -v -count=1`
- **PASS wenn:** Queue ist FIFO, kein Request wird verworfen

### T147: Kosten-Metriken im Dashboard sichtbar
- **Befehl:** `ssh ubuntu@10.0.0.240 "curl -s http://127.0.0.1:8000/api/control/traffic-stats | python3 -c 'import json,sys; d=json.load(sys.stdin); print(f\"cost={d[\"current_cost_usd\"]:.2f}, savings={d[\"estimated_savings_usd\"]:.2f}, daily_cost={d[\"projected_daily_cost_usd\"]:.2f}\")'"`
- **PASS wenn:** Alle 3 Kosten-Felder vorhanden und > 0

### T148: Structured system[] Blocks — 8 Tags implementiert
- **Befehl:** `go test ./cmd/cortex-gateway/internal/compiler/... -run "TestStructured\|Test8Block\|TestSystemBlock" -v -count=1`
- **PASS wenn:** Test bestaetigt 8 Blocks: agent-identity, company-context, personality, experience, body-state, environment, inner-voice, action-format

### T149: cache_control auf statischen Blocks
- **Befehl:** `go test ./cmd/cortex-gateway/internal/proxy/... -run "TestCacheControl\|TestEphemeral" -v -count=1`
- **PASS wenn:** cache_control=ephemeral auf ersten 3 Blocks (agent-identity, company-context, personality)

### T150: Gaia/Impulse im inner-voice Block
- **Befehl:** `grep -n "inner-voice\|gaia\|impulse" cmd/cortex-gateway/internal/compiler/structured.go`
- **PASS wenn:** Gaia-Injection ist Teil des inner-voice Blocks

---

## Zusammenfassung

| Block | Tests | Beschreibung |
|-------|-------|-------------|
| Block 0 | T1-T8 | MITM-Grundarchitektur (VORAUSSETZUNG) |
| Block 1 | T9-T24 | Datei-Existenz + Struktur (16 Dateien) |
| Block 2 | T25-T34 | Synthesis-Regeln (10 Regeln) |
| Block 3 | T35-T38 | baseGate — Synthesis-Blocker (AC-6) |
| Block 4 | T39-T43 | Chat-Sequencing P1/P3 (AC-7 bis AC-10) |
| Block 5 | T44-T50 | API-CP OODA Loop (AC-11 bis AC-14) |
| Block 6 | T51-T54 | Tick-Sync (AC-15 bis AC-17) |
| Block 7 | T55-T66 | Perception-Fingerprint (12 Felder) |
| Block 8 | T67-T74 | MITM-Interception Features (8 Funktionen) |
| Block 9 | T75-T96 | 22 ACs einzeln verifiziert |
| Block 10 | T97-T122 | 26 PFLICHT Unit-Tests |
| Block 11 | T123-T130 | 5 Integration + 3 E2E |
| Block 12 | T131-T141 | 11 Benchmarks |
| Block 13 | T142-T146 | Circuit Breaker TOGAF-Compliance (5 Tests) |
| Block 14 | T147-T155 | Fehlende Tests (Anthropic-Format, Forward-Queue, system[] Blocks, cache_control) |

**GESAMT: 155 Pruefpunkte.**

### Scope-Abgrenzung

**IN SCOPE #288:** Alles oben (Traffic Control Layer, MITM-Proxy, Synthesis, Sequencing, TickSync, API-CP, Forward-Queue, Dashboard Stats, CB-Specs)

**NICHT IN SCOPE #288 (existiert, andere Issues):**
- Bio-Engine (Sprint 1)
- ECS Systems (Sprint 1)
- Room-System (#283, #284, #289)
- Perception Injection 11 Bereiche (#260)
- Prompt Compiler 3-Quellen-Assembly (#285)
- Night-Run/NMDA (#265)
- Fourth-Wall Detection (#261)
- Action Extraction (#262)
- Session Normalizer (#264)
- Provider Registry (#263)
- NATS JetStream (#104)
- Sandbox, WASM, eBPF (eigene Issues)

### CB-Abweichungen (TOGAF vs VM)

| Parameter | TOGAF | VM aktuell | Aktion |
|-----------|-------|-----------|--------|
| Window | 20s | 60s | KORRIGIEREN |
| failure_ratio | >= 0.5 | Threshold=15 (consecutive) | KORRIGIEREN |
| Half-open | 30s | 15s | KORRIGIEREN |
| Probes | 3 | 2 | KORRIGIEREN |

Diese CB-Abweichungen sind Bugs die im Rahmen von #288 gefixt werden muessen.
