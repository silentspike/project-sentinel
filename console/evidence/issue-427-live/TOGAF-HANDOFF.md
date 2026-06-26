# #427 — TOGAF-Handoff (HAUPTSESSION-ONLY, NICHT im Worker-PR)

Dieser Worker-PR enthaelt **keinen** TOGAF-HTML-Edit (weder DE-SSOT `/home/jan/togaf-llm-architecture-guide.html`
noch Repo-Kopie `docs/architecture/togaf-architecture-guide.html`). Der TOGAF-Owner ist die Hauptsession
([[feedback-togaf-html-owner]]); ich uebergebe die Entscheidung an Control.

## Bewusst-NICHT-gewaehlt-Check (Cluster 05b) — BESTANDEN
#427 ist voll konsistent mit Cluster 05b "Deliberately NOT chosen: Prometheus/OpenTelemetry":
- Cost-Zeitreihe lebt EINMAL als `AgentLlmUsage`-Event im Event-Store (SSOT), gelesen per CQRS-Projektion (1:n).
- KEIN Prometheus-Server (`:9090` = `sentinel-daemon`-Text-Exporter, live bestaetigt via `ss -tlnp`).
- KEIN OTLP/GenAI-Export, KEIN zweiter Puffer/Ring im Dashboard (dashboard liest projection.db read-only).

## Empfehlung (Hauptsession entscheidet)
Cluster 05b beschreibt das "eigene Telemetrie-System (Event-Store-Projektion, kein externer Stack)" + die
Consumer-Matrix ("kein Byte ohne Konsument") bereits als ZIEL-Architektur (vgl. #381/#430-Eintraege in 05b).
Unter der Regel [[feedback-togaf-is-target-architecture]] ("ein im Ziel schon beschriebenes Feature umzusetzen
aendert das TOGAF NICHT") ist die per-agent/tier-Cost-Zeitreihe eine **Instanz** dieses bereits beschriebenen
Musters → **wahrscheinlich kein struktureller Edit noetig, hoechstens eine Consumer-Matrix-Zeile**
(`AgentLlmUsage → cost-Projektion → CostView`). Falls Control entscheidet, dass es eine echte Ziel-Erweiterung
ist (neue Datenebene), dann **beide Kopien sprachgetrennt** editieren (DE-SSOT bleibt Deutsch, NIE `cp` SSOT→Repo).

## #395-Kopplung (dokumentiert)
Der `tier`-Label wird bis zum Merge von #395 (model tiering, OPEN) per Fallback aus `EffectiveModel` abgeleitet
(`haiku→low`/`sonnet→mid`/`opus→high`, sonst `unknown`; synthesis/apicp/intercept als eigene Tiers). Nach #395
kann `resolveTier` durch das explizite `tier`-Feld ersetzt werden.
