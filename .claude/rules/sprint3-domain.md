# Sprint 3 - Domain Knowledge (LLM Bridge)

## Architektur-Ueberblick

Cortex Gateway ist ein transparenter Go HTTP-Proxy zwischen Agent-Sessions und LLM-Providern.
Er faengt jede Anfrage ab, reichert sie mit Wahrnehmungsdaten an (Perception Injection) und
filtert Antworten (Fourth-Wall Detection) - ohne dass der Agent davon weiss.

## Cortex Gateway Pipeline (7 Steps)

```
Agent-Session
  → Cortex Gateway (Go, Port 8080)
    → [1] Provider-Registry waehlt LLM-Backend (proxy/provider.go)
    → [2] Prompt Compiler modell-optimiert (compiler/compiler.go)
    → [3] Perception Injection ([SYSTEM_INJECTION] Block, perception.rs)
    → [4] An LLM-Provider senden (proxy/claude.go, proxy/ollama.go)
  ← LLM-Response
    ← [5] Session Normalizer vereinheitlicht (normalizer/normalizer.go)
    ← [6] Fourth-Wall Check (detection/fourth_wall.go + judge.go)
    ← [7] Action Extraction + Capability Detection (extraction/, capability/)
  → Matrix-Event an ECS-Kern via Zenoh
```

## Go Package Layout

```
cmd/cortex-gateway/
  main.go                    - Entry Point, HTTP Server, Graceful Shutdown
  internal/
    proxy/                   - Provider Interface, Registry, Claude/Ollama Backends, HTTP Handler
    normalizer/              - Claude/Ollama Response → NormalizedResponse
    compiler/                - Modell-spezifische Prompt-Kompilation
    extraction/              - Emotion/Intent-Erkennung aus LLM-Responses
    capability/              - Provider Feature Maps + Fallback-Strategien
    detection/               - Fourth-Wall Regex + LLM-Judge + Re-Generation
    control/                 - Runtime Config API (GET/PATCH config, Provider Switch)
```

## Perception Injection (Rust → Go)

```
[SYSTEM_INJECTION]
CIRCADIAN: 11:42 (Du arbeitest seit 4h konzentriert)
KOERPER: Hunger (85%). Dein Magen krampft. Blase (90%).
ENVIRONMENT: Kaffeeduft. 22.5°C, stickig.
AKUSTIK: Lebhafte Unterhaltungen.
ANWESEND: Max (konzentriert), Sophie (telefoniert).
IMPULS: Dringendes Beduerfnis, Pause zu machen.
[/SYSTEM_INJECTION]
```

### Schwellenwerte (SSOT: perception.rs)
- Hunger: >90 schwindelig, >80 Magen krampft, >70 koenntest essen
- Blasendrang: >90 JETZT, >80 Dringend, >60 bald Pause
- Energie: <20 kaum Augen offen, <40 muede
- Stress: >80 Herzrasen, >60 unter Druck
- Koffein-Entzug: caffeine_mg < 20.0 UND caffeine_tolerance > 0.3
- Akustik: 0-35 Stille, 36-50 Normal, 51-65 Lebhaft, >65 Laut
- Impuls-Prioritaet: Toilette > Hunger > Muedigkeit > Sozial > Ruhe

## Fourth-Wall Detection

### 15 Regex Patterns (Stufe 1, <1ms)
Erkennt KI-Selbsterkenntnis: "ich bin eine KI", "als Sprachmodell", "meine Trainingsdaten", etc.

### LLM-Judge (Stufe 2, bei Regex-Match)
Kleines/schnelles Modell prueft ob Regex-Match tatsaechlich ein Break ist.
False-Positive Beispiel: "Ich bin nicht real begeistert" → Regex matcht, Judge korrigiert.

### Re-Generation
Bei bestaetigtem Break: Correction-Prompt + niedrigere Temperature (0.3) fuer stabiles Re-Generation.

## Provider Interface

```go
type Provider interface {
    Name() string
    Send(ctx context.Context, req *LLMRequest) (*LLMResponse, error)
    HealthCheck(ctx context.Context) error
}
```

### Registry
- Thread-safe (sync.RWMutex)
- Primary Provider + Failover
- Runtime-wechselbar via Control Plane

### Bekannte Provider
- **Claude**: Anthropic Messages API, anthropic-version Header, Full Bio
- **Ollama**: /api/chat Format, localhost:11434, Destillierter Prompt fuer 7B

## Control Plane (Port 8081)

| Endpoint | Method | Zweck |
|----------|--------|-------|
| /control/config | GET | Aktuelle Config lesen |
| /control/config | PATCH | Config-Werte aendern (temperature, max_tokens, rate_limit) |
| /control/provider | POST | Primary Provider wechseln |

## Metriken (Prometheus, /metrics auf Port 8080)

| Metrik | Typ | Beschreibung |
|--------|-----|-------------|
| sentinel_proxy_requests_total | CounterVec | Proxy-Requests nach Provider + Status |
| sentinel_proxy_latency_seconds | HistogramVec | Proxy-Latenz nach Provider |
| sentinel_fourth_wall_detected_total | Counter | Erkannte Fourth-Wall Breaks |
| sentinel_fourth_wall_false_positive_total | Counter | Vom Judge korrigierte False-Positives |
| sentinel_fourth_wall_regen_seconds | Histogram | Re-Generation Latenz |

## Konfiguration

**SSOT:** `config/cortex-gateway.toml`
- Server: Ports (8080/8081), Timeouts
- Providers: Claude (API Key via Env), Ollama (localhost)
- Pipeline: Temperature, Feature-Flags
- Metriken: Pfad, Enabled

## Dependencies (Go)

- `github.com/prometheus/client_golang` - Metriken
- stdlib: `net/http`, `encoding/json`, `log/slog`, `regexp`, `sync`

## Naming Conventions (Sprint 3)

| Konzept | Go Identifier | Beispiel |
|---------|--------------|---------|
| Provider | PascalCase Interface | `Provider`, `ClaudeProvider` |
| Handler | PascalCase Struct | `Handler`, `Plane` |
| Config | PascalCase Struct | `Config`, `ConfigSnapshot` |
| Metriken | snake_case String | `sentinel_proxy_requests_total` |
| Patterns | camelCase Var | `fourthWallPatterns` |
| Package | lowercase | `proxy`, `detection`, `control` |
