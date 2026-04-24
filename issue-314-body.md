## Kontext

Aus der #279-Umsetzung wurde die Haiku-/Model-Policy bewusst herausgeschnitten. #279 war Daemon-Resilience-Scope; Modellwahl gehoert in die Gateway-/Inference-Schicht, nicht in den deterministischen ECS-Daemon.

Aktueller Ist-Zustand:

- `sentinel-daemon` sendet fuer Agent-Runtime kein hartes Modell (`model=""`)
- interne Runtime-Requests laufen ueber `POST /internal/llm`
- externe MITM-Kompatibilitaet laeuft ueber `POST /v1/messages`
- der laufende Gateway-Default fuer `claude-code` ist aktuell `claude-opus-4-6`
- Observability zeigt heute Provider, aber nicht die effektive interne Model-Policy

## Scope

- zentrale Gateway-/Inference-Policy fuer interne `agent_runtime` Requests
- Haiku als interner Runtime-Default fuer Agent-Traffic
- strikte Request-Klassifikation: nur positive numerische Agent-IDs, keine Platform-/Service-/Analysepfade
- keine Daemon-Seiten-Policy
- redigierte Observability fuer `provider`, `policy`, `effective_model`, `request_class`
- Regressionstest fuer Trennung von internem Runtime-Pfad und externem MITM-Pfad

## Out of Scope

- kein hartes Modell-Pinning im `sentinel-daemon`
- keine Aenderung am externen `/v1/messages`-MITM-Contract
- keine umfassende provider-agnostische Modell-Taxonomie fuer alle Service-Klassen
- keine Erweiterung auf Platform-Controlplane-Defaults, solange nicht separat spezifiziert
- keine #279-Daemon-Hardening-Aenderungen

## Acceptance Criteria

- [ ] AC-1: Interne `agent_runtime`-LLM-Requests bekommen ueber Gateway/Inference standardmaessig Haiku als effektive Model-Policy.
- [ ] AC-2: Der Daemon enthaelt kein hartes `AGENT_MODEL_HAIKU`-Pinning und bleibt bei Gateway-/Policy-Default oder einem expliziten Request-Override.
- [ ] AC-3: Runtime-Evidence auf `10.0.0.240` zeigt mindestens einen echten `agent_runtime`-Forward mit effektivem Haiku-Modell.
- [ ] AC-4: Externe MITM-/`/v1/messages`-Pfade werden durch die interne Agent-Default-Policy nicht regressiert.
- [ ] AC-5: Observability zeigt `request_class`, `provider`, `policy_source` und `effective_model` redigiert und ohne Secrets.
- [ ] AC-6: Tests und VM-Smoke belegen die Trennung von internem Runtime-Default und externem Compatibility-Pfad.

## Benchmarks

| Feld | Beschreibung |
|------|--------------|
| **Neue Metriken** | `gateway.request_classify_and_policy_resolve`, `gateway.response_log_enriched_append` |
| **Performance-Budget** | Klassifikation+Policy `< 5us/op`, Response-Log-Append `< 10us/op` |
| **Tier** | Tier 1: Go Bench lokal, Tier 2: VM-Smoke mit System-Monitoring |
| **Betroffene Sprachen** | Go |
| **Bestehende Benchmarks betroffen?** | Nein, neue Go-Benchmarks im Gateway |

## Verify-Ideen

- Gateway-Journal mit `request_class`, `provider`, `policy_source`, `effective_model`
- `GET /control/traffic-stats` zeigt aktive Runtime-Policy
- `GET /control/traffic-responses` zeigt redigierte Runtime-Eintraege mit Modell
- Regressionstest fuer `/internal/llm` vs `/v1/messages`
- Runtime-Nachweis auf `10.0.0.240` mit kontrolliertem Agent-Forward
