# Testmatrix #288 v3 — Anti-Schummel-Edition

**Datum:** 2026-03-30
**Commit:** 4e69d4f + e4f8769
**Architektur:** claude -p → Gateway /v1/messages (MITM) → anthropic-direct → api.anthropic.com
**VM:** ubuntu@10.0.0.240

**Parity-Hinweis (2026-04-03):**
Diese Matrix beschreibt den verifizierten MITM-Stand auf `e4f8769` plus laufender VM.
Sie darf nicht als Beweis dafuer gelesen werden, dass GitHub `origin/main` heute bereits parity zu diesem Stand ist.
Diese gesonderte Parity-Luecke wird in `#298` verfolgt.

---

## Anti-Schummel-Regeln

1. **Kein PASS ohne Command + exakten Output** im Ergebnis-Markdown
2. **GUI-Tests: playwright-cli screenshot** — kein curl auf API
3. **Runtime-Tests: ganzer Datenfluss** — nicht nur grep auf ein Keyword
4. **1 Test = 1 Task** — kein Batching
5. **Code-Lesen ist KEIN Evidence** fuer Funktionalitaet (nur fuer Existenz)
6. **Jeder Test hat eine Kategorie:** CODE / UNIT / RUNTIME / GUI / E2E

---

## Test-Kategorien erklaert

| Kategorie | Was | Evidence-Pflicht | Schummeln moeglich? |
|-----------|-----|------------------|---------------------|
| CODE | Datei/Funktion existiert | grep Output reinkopieren | Nein (grep luegt nicht) |
| UNIT | Go-Test laeuft | Voller `go test -run X -v` Output | Nein (Test-Output ist Beweis) |
| RUNTIME | Feature funktioniert auf VM | ssh Command + Output reinkopieren | Moeglich wenn nur grep — deshalb GANZEN Datenfluss zeigen |
| GUI | Feature sichtbar im Dashboard | playwright-cli screenshot Pfad | Nein (Screenshot luegt nicht) |
| E2E | MITM-Datenfluss end-to-end | Request senden + Gateway-Log + Response zeigen | Nein (3 Beweise zusammen) |

---

## BLOCK 0: MITM-Grundarchitektur [8 Tests]

Ohne PASS hier sind alle anderen Tests sinnlos.

### T1 [E2E] MITM-Proxy funktioniert: claude -p → Gateway → Anthropic API
```
SENDEN:  ssh ubuntu@10.0.0.240 "ANTHROPIC_BASE_URL=http://127.0.0.1:8080 NO_PROXY=127.0.0.1 claude -p 'Antworte exakt mit PONG.' --output-format json 2>&1 | head -20"
PRUEFEN: ssh ubuntu@10.0.0.240 "journalctl -u sentinel-gateway --since '1 min ago' --no-pager | grep -E 'provider|anthropic-direct|/v1/messages' | tail -5"
```
- **PASS wenn:** claude -p bekommt "PONG" UND Journal zeigt provider=anthropic-direct
- **BLOCKED wenn:** Rate Limit aktiv
- **Evidence:** Beide Outputs komplett reinkopieren

### T2 [E2E] /v1/messages Endpoint nimmt Anthropic-Request an
```
SENDEN:  ssh ubuntu@10.0.0.240 "curl -s -X POST http://127.0.0.1:8080/v1/messages -H 'Content-Type: application/json' -H 'x-api-key: test-key' -H 'anthropic-version: 2023-06-01' -d '{\"model\":\"claude-opus-4-6\",\"max_tokens\":100,\"messages\":[{\"role\":\"user\",\"content\":\"say PONG\"}]}' | head -20"
PRUEFEN: ssh ubuntu@10.0.0.240 "journalctl -u sentinel-gateway --since '30s ago' --no-pager | tail -10"
```
- **PASS wenn:** Gateway akzeptiert Request (kein 404/405) UND loggt den Request
- **Evidence:** curl Response + Journal Output

### T3 [UNIT] Header-Passthrough: Auth-Headers werden durchgereicht
```
go test ./cmd/cortex-gateway/internal/proxy/... -run "TestPassthrough\|TestHeader\|TestAnthropicPassthrough" -v -count=1 2>&1
```
- **PASS wenn:** Test existiert UND PASS — Authorization, x-api-key, anthropic-version, anthropic-beta
- **Evidence:** Voller Test-Output

### T4 [UNIT] system[] Blocks bleiben strukturiert (kein Flattening auf MITM-Pfad)
```
go test ./cmd/cortex-gateway/internal/proxy/... -run "TestStructuredSystem\|TestSplitAnthropicMessages\|TestPipelineStructuredSystem" -v -count=1 2>&1
```
- **PASS wenn:** SystemBlocks Array wird als system[] JSON Array weitergegeben
- **Evidence:** Voller Test-Output

### T5 [UNIT] PreferredProvider = anthropic-direct fuer /v1/messages Requests
```
go test ./cmd/cortex-gateway/internal/proxy/... -run "TestAnthropicDecode\|TestDecodeAnthropic\|TestPipelineAnthropicMessages" -v -count=1 2>&1
```
- **PASS wenn:** Request auf /v1/messages setzt PreferredProvider = "anthropic-direct"
- **Evidence:** Voller Test-Output

### T6 [UNIT] Anthropic-kompatibles Response-Format
```
go test ./cmd/cortex-gateway/internal/proxy/... -run "TestBuildAnthropicMessage\|TestAnthropicResponse\|TestPipelineAnthropicMessages" -v -count=1 2>&1
```
- **PASS wenn:** Response: {type:"message", role:"assistant", content:[{type:"text"}], usage, stop_reason}
- **Evidence:** Voller Test-Output

### T7 [RUNTIME] Gateway Health + Services aktiv
```
ssh ubuntu@10.0.0.240 "sudo systemctl is-active sentinel-gateway sentinel-daemon"
ssh ubuntu@10.0.0.240 "curl -s http://127.0.0.1:8080/health"
ssh ubuntu@10.0.0.240 "curl -s http://127.0.0.1:8081/health 2>/dev/null || echo 'no control health'"
```
- **PASS wenn:** Beide Services "active", Health "ok"
- **Evidence:** Alle 3 Outputs

### T8 [RUNTIME] Traffic-Stats Endpoint liefert Daten
```
ssh ubuntu@10.0.0.240 "curl -s http://127.0.0.1:8081/control/traffic-stats"
```
- **PASS wenn:** JSON mit synthesis_count, synthesis_rate, forward_calls, apicp etc.
- **Evidence:** Voller JSON-Output

---

## BLOCK 1: Datei-Existenz + Schluessel-Strukturen [16 Tests]

### T9-T14 [CODE] 6 neue Dateien
Pro Datei: `test -f PFAD && wc -l PFAD && head -5 PFAD`

| # | Datei | Erwartung |
|---|-------|-----------|
| T9 | cmd/cortex-gateway/internal/control/plane.go | TrafficStats Handler |
| T10 | cmd/cortex-gateway/internal/synthesis/engine.go | Synthesis Engine |
| T11 | cmd/cortex-gateway/internal/sequencing/queue.go | P1/P3 Sequencer |
| T12 | cmd/cortex-gateway/internal/synthesis/fingerprint.go | Fingerprint Parser |
| T13 | cmd/cortex-gateway/internal/apicp/observer.go | OODA Observer |
| T14 | cmd/cortex-gateway/internal/ticksync/buffer.go | Tick-Sync Buffer |

### T15-T22 [CODE] 8 erweiterte Dateien
Pro Datei: grep nach Schluesselstruktur

| # | Datei | grep-Pattern | Erwartung |
|---|-------|-------------|-----------|
| T15 | proxy/pipeline.go | "Step 7.5" | Inbound TC Step eingefuegt |
| T16 | proxy/provider.go | "type Provider interface" | Provider Interface |
| T17 | main.go | "synthEngine\|apicpObserver\|Sequencer" | TC-Komponenten initialisiert |
| T18 | config.rs | "TrafficControlConfig" | Config Struct |
| T19 | daemon.toml | "traffic_control" | Config Sektion |
| T20 | types.rs | "synth_fingerprint" | Fingerprint in Perception |
| T21 | llm_bridge.rs | "synth_fp" | Fingerprint in Metadata |
| T22 | redb lib.rs | "api_patterns" | API-CP Table |

### T23-T24 [GUI] Dashboard Traffic Control Panel
```
T23: playwright-cli -s=t23 open http://10.0.0.240:8000 --headed
     playwright-cli -s=t23 screenshot
     playwright-cli -s=t23 close
T24: Navigiere zu Control Tab, Screenshot vom Traffic Stats Panel
     playwright-cli -s=t24 open http://10.0.0.240:8000
     playwright-cli -s=t24 click [Control-Tab]
     playwright-cli -s=t24 screenshot
     playwright-cli -s=t24 close
```
- **PASS wenn:** Screenshot zeigt Synthesis-Rate, Costs, API-CP Patterns im Control Panel
- **Evidence:** Screenshot-Pfad + Screenshot-Inhalt beschreiben

---

## BLOCK 2: Synthesis-Regeln [10 Tests]

Jede Regel hat: UNIT-Test + RUNTIME-Check (journalctl fuer welche Regeln feuern)

### T25-T34 [UNIT+RUNTIME]
Pro Regel:
```
UNIT:    go test ./cmd/cortex-gateway/internal/synthesis/... -run "TestNAME" -v -count=1
RUNTIME: ssh ubuntu@10.0.0.240 "journalctl -u sentinel-gateway --since '2h ago' --no-pager | grep 'synthesis match' | grep 'REGELNAME' | head -3"
```

| # | Regel | Unit-Test | Runtime-grep |
|---|-------|-----------|-------------|
| T25 | Bio Hunger | TestBioHungerFires | bio_hunger |
| T26 | Bio Blase | TestBioBladderUsesModuloTarget | bio_bladder |
| T27 | Bio Energie | (TestBioEnergy oder implizit) | bio_energy |
| T28 | Bio Koffein | (TestCaffeine oder implizit) | bio_caffeine |
| T29 | Idle allein | TestRoutineIdleAlone | routine_idle_alone |
| T30 | Idle + Anwesende | TestRoutineIdleWithPresence | routine_idle_with_presence |
| T31 | Temperatur | TestPhysicsTempHigh | physics_temp |
| T32 | Laerm | TestPhysicsNoiseHigh | physics_noise |
| T33 | Circadian Morgen | TestCircadianMorning | circadian_morning |
| T34 | Circadian Mittag | TestCircadianLunch | circadian_lunch |

- **PASS wenn:** Unit-Test PASS UND mindestens 1 Runtime-Match (oder erklaerbar warum nicht — z.B. simHour passt nicht)
- **Evidence:** Unit-Test Output + journalctl Output

---

## BLOCK 3: baseGate — Synthesis-Blocker [4 Tests]

### T35-T38 [UNIT]
```
go test ./cmd/cortex-gateway/internal/synthesis/... -run "TestBioHungerBlockedByHeardMetadata" -v -count=1
go test ./cmd/cortex-gateway/internal/synthesis/... -run "TestBioHungerBlockedByChaos" -v -count=1
go test ./cmd/cortex-gateway/internal/synthesis/... -run "TestBioBladderBlockedByAddressed" -v -count=1
go test ./cmd/cortex-gateway/internal/synthesis/... -run "TestImpulseBypassesSynthesis" -v -count=1
```
- **PASS wenn:** Alle 4 Tests PASS — Synthesis wird blockiert bei heard/chaos/addressed/impulse
- **Evidence:** Voller Output pro Test

---

## BLOCK 4: Chat-Sequencing P1/P3 [5 Tests]

### T39-T43 [UNIT]
```
T39: go test ./cmd/cortex-gateway/internal/sequencing/... -run "TestP1Forward" -v -count=1
T40: go test ./cmd/cortex-gateway/internal/sequencing/... -run "TestP3WaitsForP1" -v -count=1
T41: go test ./cmd/cortex-gateway/internal/proxy/... -run "TestPipelineSequencingInjectsP1Context" -v -count=1
T42: go test ./cmd/cortex-gateway/internal/sequencing/... -run "TestMultipleP3s" -v -count=1
T43: go test ./cmd/cortex-gateway/internal/sequencing/... -run "TestP3Timeout" -v -count=1
```
- **Evidence:** Voller Output pro Test

### T43b [RUNTIME] Chat-Sequencing Events auf VM
```
ssh ubuntu@10.0.0.240 "journalctl -u sentinel-gateway --since '6h ago' --no-pager | grep -E 'p1 active|p1 completed|p3 released|context.*inject' | tail -10"
```
- **PASS wenn:** P1/P3 Events sichtbar ODER BLOCKED (Rate Limit = kein Forward = kein Chat)
- **Evidence:** journalctl Output

---

## BLOCK 5: API-CP OODA Loop [7 Tests]

### T44-T49 [UNIT]
```
T44: go test ./cmd/cortex-gateway/internal/apicp/... -run "TestRecordAndStats" -v -count=1
T45: go test ./cmd/cortex-gateway/internal/apicp/... -run "TestConfidence" -v -count=1
T46: go test ./cmd/cortex-gateway/internal/apicp/... -run "TestSuggestions" -v -count=1
T47: go test ./cmd/cortex-gateway/internal/proxy/... -run "TestPipelineAPICPLearnedPattern" -v -count=1
T48: go test ./cmd/cortex-gateway/internal/apicp/... -run "TestShouldProbe|TestApplyProbeResult" -v -count=1
T49: go test ./cmd/cortex-gateway/internal/apicp/... -run "TestEvolutionDegradation" -v -count=1
```

### T50 [RUNTIME] API-CP Persistence: Daemon hat Patterns
```
ssh ubuntu@10.0.0.240 "curl -s http://127.0.0.1:8084/operator/apicp/snapshot | python3 -c 'import json,sys; d=json.load(sys.stdin); print(json.dumps({\"patterns\":len(d.get(\"patterns\",[])),\"synth_count\":d.get(\"synth_count\",0)}, indent=2))'"
```
- **PASS wenn:** patterns > 0 UND synth_count > 0
- **Evidence:** JSON Output

---

## BLOCK 6: Tick-Sync [4 Tests]

### T51-T54 [UNIT+RUNTIME]
```
T51: go test ./cmd/cortex-gateway/internal/ticksync/... -run "TestHoldAndFlush" -v -count=1
T52: go test ./cmd/cortex-gateway/internal/ticksync/... -run "TestPriorityOrdering" -v -count=1
T53: ssh ubuntu@10.0.0.240 "journalctl -u sentinel-gateway --since '1h ago' --no-pager | grep -c 'tick_sync.*flush'"
T54: go test ./cmd/cortex-gateway/internal/ticksync/... -run "TestDisabledBuffer|TestSetEnabled" -v -count=1
```
- **Evidence:** Test-Outputs + Flush-Count von VM

---

## BLOCK 7: Perception-Fingerprint [2 Tests statt 12]

Alle 12 Felder werden in EINEM Test geprueft — kein Grund 12 einzelne Tasks zu machen:

### T55 [UNIT] Fingerprint-Parser alle 12 Felder
```
go test ./cmd/cortex-gateway/internal/synthesis/... -run "TestParseFingerprint|TestParsePartialFields|TestEmptyFingerprint" -v -count=1
```
- **PASS wenn:** Alle 3 Tests PASS — H/E/B/S/C/SN/R/P/CH/HR/T/IM korrekt geparst
- **Evidence:** Voller Output

### T56 [RUNTIME] Fingerprint wird auf VM erzeugt
```
ssh ubuntu@10.0.0.240 "journalctl -u sentinel-gateway --since '30 min ago' --no-pager | grep 'synth_fp\|fingerprint' | head -3"
```
- **PASS wenn:** Fingerprint-Strings in Logs sichtbar (z.B. "H3|E7|B2|...")
- **Evidence:** journalctl Output

---

## BLOCK 8: MITM-Interception Features [5 Tests]

### T57 [UNIT] Request Hold + Resolve
```
go test ./cmd/cortex-gateway/internal/intercept/... -run "TestAwaitRequestDecision" -v -count=1
```

### T58 [UNIT] Request Modify (Pipeline Integration)
```
go test ./cmd/cortex-gateway/internal/proxy/... -run "TestPipelineManualInterceptModify" -v -count=1
```

### T59 [UNIT] Response Hold + Replace
```
go test ./cmd/cortex-gateway/internal/intercept/... -run "TestAwaitResponseDecision" -v -count=1
go test ./cmd/cortex-gateway/internal/proxy/... -run "TestPipelineManualResponseReplace" -v -count=1
```

### T60 [RUNTIME] InterceptMode + Pending auf VM
```
ssh ubuntu@10.0.0.240 "curl -s http://127.0.0.1:8081/control/traffic-stats | python3 -c 'import json,sys; d=json.load(sys.stdin); print(json.dumps({\"intercept_mode\":d[\"intercept_mode\"],\"pending_intercepts\":d[\"pending_intercepts\"],\"pending_response_intercepts\":d[\"pending_response_intercepts\"],\"response_log_entries\":d[\"response_log_entries\"]}, indent=2))'"
```
- **PASS wenn:** intercept_mode vorhanden, response_log_entries > 0

### T61 [GUI] Intercept-Panel im Dashboard
```
playwright-cli -s=t61 open http://10.0.0.240:8000
playwright-cli -s=t61 click [Control Tab]
playwright-cli -s=t61 screenshot
playwright-cli -s=t61 close
```
- **PASS wenn:** Screenshot zeigt Intercept Mode, Pending Counts

---

## BLOCK 9: 22 ACs — KEINE Verweise, eigene Evidence [22 Tests]

Jede AC hat ihren EIGENEN Befehl und ihr EIGENES Evidence. Kein "siehe T39".

### T62: AC-1 Bio-Impulse synthetisiert
```
ssh ubuntu@10.0.0.240 "curl -s http://127.0.0.1:8081/control/traffic-stats | python3 -c 'import json,sys; d=json.load(sys.stdin); print(f\"synthesis_count={d[\"synthesis_count\"]}, rate={d[\"synthesis_rate\"]*100:.1f}%\")'"
```
- **PASS wenn:** synthesis_count > 100 UND rate > 10%

### T63: AC-2 Synthesis durchlaeuft Outbound-Pipeline
```
go test ./cmd/cortex-gateway/internal/proxy/... -run "TestPipelineSynthesisAndForwardShareOutbound" -v -count=1
```

### T64: AC-3 Synthesis per Config deaktivierbar
```
go test ./cmd/cortex-gateway/internal/synthesis/... -run "TestDisabledEngine" -v -count=1
ssh ubuntu@10.0.0.240 "grep SENTINEL_SYNTHESIS_ENABLED /etc/systemd/system/sentinel-gateway.service"
```

### T65: AC-4 Persoenlichkeitsabhaengige Templates (I vs E)
```
go test ./cmd/cortex-gateway/internal/synthesis/... -run "TestPersonalityTemplates" -v -count=1
```

### T66: AC-5 Circadian + Physics Synthesis
```
go test ./cmd/cortex-gateway/internal/synthesis/... -run "TestCircadian|TestPhysics" -v -count=1
```

### T67: AC-6 Synthesis NIE bei Chat/Heard/Chaos/Impulse
```
go test ./cmd/cortex-gateway/internal/synthesis/... -run "TestBlocked|TestImpulse" -v -count=1
```

### T68: AC-7 P1 sofort forwarded
```
go test ./cmd/cortex-gateway/internal/sequencing/... -run "TestP1Forward" -v -count=1
```

### T69: AC-8 P3 gequeued bis P1 antwortet
```
go test ./cmd/cortex-gateway/internal/sequencing/... -run "TestP3WaitsForP1" -v -count=1
```

### T70: AC-9 P1-Antwort in P3-Kontext injiziert
```
go test ./cmd/cortex-gateway/internal/proxy/... -run "TestPipelineSequencingInjectsP1Context" -v -count=1
```

### T71: AC-10 Timeout 5s
```
go test ./cmd/cortex-gateway/internal/sequencing/... -run "TestP3Timeout" -v -count=1
ssh ubuntu@10.0.0.240 "curl -s http://127.0.0.1:8081/control/traffic-stats | python3 -c 'import json,sys; d=json.load(sys.stdin); print(f\"p3_timeout_ms={d[\"p3_timeout_ms\"]}\")'"
```
- **PASS wenn:** Test PASS UND p3_timeout_ms = 5000

### T72: AC-11 API-CP beobachtet alle Calls
```
ssh ubuntu@10.0.0.240 "curl -s http://127.0.0.1:8081/control/traffic-stats | python3 -c 'import json,sys; d=json.load(sys.stdin); print(f\"active_patterns={d[\"active_patterns\"]}\")'"
```
- **PASS wenn:** active_patterns > 0

### T73: AC-12 Confidence > 90% → Synthesis
```
ssh ubuntu@10.0.0.240 "curl -s http://127.0.0.1:8081/control/traffic-stats | python3 -c 'import json,sys; d=json.load(sys.stdin); print(f\"promoted={d[\"apicp\"][\"promoted_patterns\"]}, suggestions={d[\"apicp_suggestion_count\"]}\")'"
```
- **PASS wenn:** promoted_patterns >= 1

### T74: AC-13 Evolution-Change halbiert Confidences
```
go test ./cmd/cortex-gateway/internal/apicp/... -run "TestEvolutionDegradation" -v -count=1
```

### T75: AC-14 Stichproben-Verifikation
```
go test ./cmd/cortex-gateway/internal/apicp/... -run "TestShouldProbe|TestApplyProbeResult" -v -count=1
```

### T76: AC-15 Tick-Sync auf Tick-Grenzen
```
go test ./cmd/cortex-gateway/internal/ticksync/... -run "TestHoldAndFlush" -v -count=1
ssh ubuntu@10.0.0.240 "journalctl -u sentinel-gateway --since '1h ago' --no-pager | grep -c 'tick_sync.*flush'"
```
- **PASS wenn:** Test PASS UND Flush-Count > 0

### T77: AC-16 P1 > P3 Ordering
```
go test ./cmd/cortex-gateway/internal/ticksync/... -run "TestPriorityOrdering" -v -count=1
```

### T78: AC-17 Tick-Sync deaktivierbar
```
go test ./cmd/cortex-gateway/internal/ticksync/... -run "TestDisabledBuffer" -v -count=1
```

### T79: AC-18 Feature-gated, komplett deaktivierbar
```
grep -c "SENTINEL_SYNTHESIS_ENABLED\|SENTINEL_SEQUENCING_ENABLED\|SENTINEL_TICK_SYNC_ENABLED\|SENTINEL_APICP_ENABLED" cmd/cortex-gateway/main.go
ssh ubuntu@10.0.0.240 "grep -c 'SENTINEL_.*_ENABLED' /etc/systemd/system/sentinel-gateway.service"
```
- **PASS wenn:** 4 Env-Vars in Code UND in systemd Unit

### T80: AC-19 Dashboard Traffic Stats
```
playwright-cli -s=t80 open http://10.0.0.240:8000
playwright-cli -s=t80 click [Control Tab]
playwright-cli -s=t80 screenshot
playwright-cli -s=t80 close
```
- **PASS wenn:** Screenshot zeigt Synthesis-Rate, Costs, API-CP Patterns, Queue-Depth

### T81: AC-20 ZERO Disk-Writes im Hot-Path
```
grep -rn "os.WriteFile\|os.Create\|ioutil.WriteFile" cmd/cortex-gateway/internal/synthesis/ cmd/cortex-gateway/internal/sequencing/ cmd/cortex-gateway/internal/ticksync/
```
- **PASS wenn:** grep findet KEINE Treffer

### T82: AC-21 Alle bestehenden Tests gruen
```
go test ./cmd/cortex-gateway/... -count=1 2>&1 | tail -20
```
- **PASS wenn:** Kein "FAIL" im Output

### T83: AC-22 Latenz-Overhead < 5ms
```
go test ./cmd/cortex-gateway/internal/proxy/... -run "TestPipelineSynthesis" -v -count=1 2>&1 | grep -i "duration\|latency\|elapsed"
go test ./cmd/cortex-gateway/internal/synthesis/... -bench "." -benchtime=1000x -count=1 2>&1 | grep "ns/op"
```
- **PASS wenn:** Synthesis-Pfad < 5ms (< 5000000 ns/op)

---

## BLOCK 10: 26 PFLICHT Unit-Tests [1 Test — Batch-Run mit einzelner Auswertung]

Statt 26 einzelne Tasks: EIN go test Run, dann jeden Test-Namen im Output pruefen.

### T84 [UNIT] Alle 26 PFLICHT-Tests in einem Run
```
go test ./cmd/cortex-gateway/... -v -count=1 -run "TestBioHunger|TestBioBladder|TestRoutineIdle|TestImpulse|TestPhysicsTemp|TestPhysicsNoise|TestCircadian|TestDisabledEngine|TestPersonalityTemplates|TestP1Forward|TestP3Waits|TestPipelineSequencing|TestP3Timeout|TestP3NoActive|TestMultipleP3|TestHoldAndFlush|TestPriorityOrdering|TestDisabledBuffer|TestParse|TestConfidence|TestSuggestions|TestEvolution|TestShouldProbe|TestApplyProbe|TestPatternLimit" 2>&1
```
- **PASS wenn:** 0 FAIL, alle 26 Test-Namen tauchen als PASS auf
- **Evidence:** Voller Output, jeden Test-Namen markieren

---

## BLOCK 11: 5 Integration-Tests + 3 E2E [8 Tests]

### T85-T89 [UNIT] Integration-Tests
```
T85: go test ./cmd/cortex-gateway/internal/proxy/... -run "TestPipelineSynthesisUsesRuleActions" -v -count=1
T86: go test ./cmd/cortex-gateway/internal/proxy/... -run "TestPipelineSequencingInjectsP1Context|TestPipelineSequencingTimeout" -v -count=1
T87: go test ./cmd/cortex-gateway/internal/proxy/... -run "TestPipelineQueueTickSyncAndSequencingIntegrate" -v -count=1
T88: go test ./cmd/cortex-gateway/internal/proxy/... -run "TestPipelineSynthesisAndForwardShareOutbound" -v -count=1
T89: go test ./cmd/cortex-gateway/internal/proxy/... -run "TestPipelineAPICPLearnedPattern|TestPipelineAPICPProbe" -v -count=1
```

### T90 [RUNTIME] E2E1: Synthesis-Rate > 30%
```
ssh ubuntu@10.0.0.240 "curl -s http://127.0.0.1:8081/control/traffic-stats | python3 -c 'import json,sys; d=json.load(sys.stdin); r=d[\"synthesis_rate\"]*100; print(f\"Rate: {r:.1f}% — PASS\" if r > 30 else f\"Rate: {r:.1f}% — FAIL\")'"
```

### T91 [RUNTIME] E2E2: Chat-Sequencing Events beobachtet
```
ssh ubuntu@10.0.0.240 "journalctl -u sentinel-gateway --since '6h ago' --no-pager | grep -c 'p1 active\|p1 completed\|p3 released'"
```
- **PASS wenn:** Count > 0 ODER BLOCKED (Rate Limit)

### T92 [RUNTIME] E2E3: API-CP hat Patterns gelernt
```
ssh ubuntu@10.0.0.240 "curl -s http://127.0.0.1:8081/control/traffic-stats | python3 -c 'import json,sys; d=json.load(sys.stdin); print(f\"patterns={d[\"active_patterns\"]}, promoted={d[\"apicp\"][\"promoted_patterns\"]}\")'
```
- **PASS wenn:** patterns > 50 UND promoted >= 1

---

## BLOCK 12: Benchmarks [4 Tests statt 11 — nur was messbar ist]

### T93 [UNIT] Synthesis + Fingerprint + Queue + TickSync + API-CP Benchmarks
```
go test ./cmd/cortex-gateway/internal/synthesis/... -bench "." -benchtime=1000x -count=1 2>&1 | grep "ns/op"
go test ./cmd/cortex-gateway/internal/sequencing/... -bench "." -benchtime=1000x -count=1 2>&1 | grep "ns/op"
go test ./cmd/cortex-gateway/internal/ticksync/... -bench "." -benchtime=1000x -count=1 2>&1 | grep "ns/op"
go test ./cmd/cortex-gateway/internal/apicp/... -bench "." -benchtime=1000x -count=1 2>&1 | grep "ns/op"
go test ./cmd/cortex-gateway/internal/forwardqueue/... -bench "." -benchtime=1000x -count=1 2>&1 | grep "ns/op"
```
- **PASS wenn:** Alle < 5ms (5000000 ns/op)

### T94 [RUNTIME] Synthesis-Rate > 30%
Gleich wie T90 — Referenz.

### T95 [RUNTIME] Kosten-Reduktion > 40%
```
ssh ubuntu@10.0.0.240 "curl -s http://127.0.0.1:8081/control/traffic-stats | python3 -c 'import json,sys; d=json.load(sys.stdin); s=d[\"estimated_savings_usd\"]; c=d[\"current_cost_usd\"]; r=s/(s+c)*100 if (s+c)>0 else 0; print(f\"Savings: \${s:.0f}, Cost: \${c:.0f}, Reduction: {r:.1f}%\")'"
```
- **PASS wenn:** Reduction > 40%

### T96 [RUNTIME] Gateway Memory < 200 MB
```
ssh ubuntu@10.0.0.240 "ps -o rss= -p \$(pgrep cortex-gate) | awk '{print \$1/1024 \" MB\"}'"
```
- **PASS wenn:** < 200 MB

---

## BLOCK 13: Circuit Breaker TOGAF-Compliance [5 Tests]

### T97 [CODE+RUNTIME] CB Window
```
grep -n "window\|Window\|WINDOW" cmd/cortex-gateway/internal/proxy/circuit_breaker.go | head -10
ssh ubuntu@10.0.0.240 "grep CB_WINDOW /etc/systemd/system/sentinel-gateway.service /etc/systemd/system/sentinel-gateway.service.d/*.conf 2>/dev/null"
```
- **TOGAF:** 20s
- **PASS wenn:** Window = 20s

### T98 [CODE+RUNTIME] CB failure_ratio
```
grep -n "ratio\|Ratio\|threshold\|Threshold\|consecutive" cmd/cortex-gateway/internal/proxy/circuit_breaker.go | head -10
ssh ubuntu@10.0.0.240 "grep CB_FAILURE /etc/systemd/system/sentinel-gateway.service"
```
- **TOGAF:** >= 0.5 (ratio-basiert)
- **PASS wenn:** Ratio-basiert mit 50% Schwelle

### T99 [CODE+RUNTIME] CB Half-open Duration
```
grep -n "halfopen\|half.open\|HalfOpen\|OPEN_SECONDS" cmd/cortex-gateway/internal/proxy/circuit_breaker.go | head -10
ssh ubuntu@10.0.0.240 "grep OPEN_SECONDS /etc/systemd/system/sentinel-gateway.service"
```
- **TOGAF:** 30s
- **PASS wenn:** Half-open = 30s

### T100 [CODE+RUNTIME] CB Probes
```
grep -n "probe\|Probe\|HALFOPEN_PROBES" cmd/cortex-gateway/internal/proxy/circuit_breaker.go | head -10
ssh ubuntu@10.0.0.240 "grep HALFOPEN_PROBES /etc/systemd/system/sentinel-gateway.service"
```
- **TOGAF:** 3 Probes
- **PASS wenn:** HalfOpenProbes = 3

### T101 [RUNTIME] CB pro Provider isoliert
```
ssh ubuntu@10.0.0.240 "curl -s http://127.0.0.1:8080/health | python3 -m json.tool"
```
- **PASS wenn:** circuit_breakers Map zeigt separate States pro Provider

---

## BLOCK 14: Neue Tests (Architektur-Vollstaendigkeit) [9 Tests]

### T102 [CODE] Anthropic-Error-Format
```
grep -n "writeAnthropicError\|anthropicErrorResponse" cmd/cortex-gateway/internal/proxy/anthropic_api.go
```

### T103 [UNIT] Synthesis-Response als Anthropic-Format
```
go test ./cmd/cortex-gateway/internal/proxy/... -run "TestPipelineAnthropicMessages" -v -count=1
```

### T104 [CODE] Judge/Regen behalten system[] Blocks
```
grep -n "SystemBlocks\|systemBlocks\|PassthroughHeaders" cmd/cortex-gateway/internal/proxy/judge_adapter.go
```

### T105 [UNIT+RUNTIME] Forward-Queue Max 3
```
go test ./cmd/cortex-gateway/internal/forwardqueue/... -run "TestManager" -v -count=1
ssh ubuntu@10.0.0.240 "curl -s http://127.0.0.1:8081/control/traffic-stats | python3 -c 'import json,sys; d=json.load(sys.stdin); print(f\"max_concurrency={d[\"max_forward_concurrency\"]}\")'"
```
- **PASS wenn:** Test PASS UND max_forward_concurrency = 3

### T106 [UNIT] Forward-Queue FIFO, kein Drop
```
go test ./cmd/cortex-gateway/internal/forwardqueue/... -run "TestLimits|TestCancel|TestSetMax" -v -count=1
```

### T107 [RUNTIME] Kosten-Metriken vorhanden
```
ssh ubuntu@10.0.0.240 "curl -s http://127.0.0.1:8081/control/traffic-stats | python3 -c 'import json,sys; d=json.load(sys.stdin); print(json.dumps({k:d[k] for k in [\"current_cost_usd\",\"estimated_savings_usd\",\"projected_daily_cost_usd\",\"avg_forward_cost_usd\"] if k in d}, indent=2))'"
```
- **PASS wenn:** Alle 4 Felder vorhanden und > 0

### T108 [UNIT] 8 strukturierte system[] Blocks
```
go test ./cmd/cortex-gateway/internal/compiler/... -run "TestStructured" -v -count=1
```
- **PASS wenn:** Test bestaetigt 8 Blocks mit Tags

### T109 [UNIT] cache_control auf statischen Blocks
```
go test ./cmd/cortex-gateway/internal/proxy/... -run "TestCacheControl|TestEphemeral|TestStructuredSystem" -v -count=1
```

### T110 [CODE] Gaia/Impulse im inner-voice Block
```
grep -n "inner.voice\|innerVoice\|InnerVoice\|impulse\|gaia" cmd/cortex-gateway/internal/compiler/structured.go | head -10
```

---

## Gesamtzaehlung

| Block | Tests | Kategorie-Mix |
|-------|-------|---------------|
| 0: MITM-Basis | T1-T8 (8) | 2 E2E, 4 UNIT, 2 RUNTIME |
| 1: Dateien | T9-T24 (16) | 14 CODE, 2 GUI |
| 2: Synthesis | T25-T34 (10) | 10x UNIT+RUNTIME |
| 3: baseGate | T35-T38 (4) | 4 UNIT |
| 4: Sequencing | T39-T43b (6) | 5 UNIT, 1 RUNTIME |
| 5: API-CP | T44-T50 (7) | 6 UNIT, 1 RUNTIME |
| 6: Tick-Sync | T51-T54 (4) | 2 UNIT, 1 RUNTIME, 1 UNIT |
| 7: Fingerprint | T55-T56 (2) | 1 UNIT, 1 RUNTIME |
| 8: Interception | T57-T61 (5) | 3 UNIT, 1 RUNTIME, 1 GUI |
| 9: 22 ACs | T62-T83 (22) | Mix aus UNIT+RUNTIME+GUI |
| 10: 26 Unit-Tests | T84 (1 Batch) | 1 UNIT (26 Tests drin) |
| 11: Integration+E2E | T85-T92 (8) | 5 UNIT, 3 RUNTIME |
| 12: Benchmarks | T93-T96 (4) | 1 UNIT, 3 RUNTIME |
| 13: CB TOGAF | T97-T101 (5) | 5x CODE+RUNTIME |
| 14: Architektur | T102-T110 (9) | 3 CODE, 4 UNIT, 2 RUNTIME |

**GESAMT: 111 Test-Aufgaben** (decken alle 155 Pruefpunkte ab, konsolidiert wo sinnvoll)
**Davon GUI-Tests: 3** (T23, T24/T61, T80)
**Davon E2E: 2** (T1, T2)
**Davon RUNTIME: ~30**
**Davon UNIT: ~55**
**Davon CODE: ~21**
