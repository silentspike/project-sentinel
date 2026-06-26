# #424 — TOGAF-Handoff (HAUPTSESSION-ONLY, NICHT im Worker-PR)

Dieser Worker-PR enthaelt **keinen** TOGAF-HTML-Edit (weder DE-SSOT noch Repo-Kopie). TOGAF-Owner =
Hauptsession/ORC ([[feedback-togaf-html-owner]]); ich uebergebe die Entscheidung an Control.

## Bezug
Issue-DoD: "add this view to TOGAF HTML Cluster 04b (Gaia Console); keep DEV-009 (Polyglot Frontend
SolidJS) consistent — MAIN SESSION ONLY."

## Empfehlung (Hauptsession entscheidet)
#424 ist das **CostView/SynthesisView-Muster** — ein read-only SolidJS-Konsole-Panel, das eine bestehende
Read-Route liest (`/api/config/agents`, #420). Es bringt **keine neue Architektur-Entscheidung**: kein
neuer Backend-Code, keine neue Datenebene, keine neue Transport-/Telemetrie-Entscheidung. Unter der Regel
[[feedback-togaf-is-target-architecture]] ("ein im Ziel schon beschriebenes Feature umzusetzen aendert das
TOGAF NICHT") ist die Org-Chart-View eine **Instanz** der bereits in Cl.04b beschriebenen Gaia-Console
(Polyglot-Frontend, Panel-getriebene Konsole).
- **Wenn Cl.04b die Panels enumeriert** (Panel-Liste / Consumer-Matrix-Stil) → **eine Zeile** ergaenzen
  (`Org Chart` Panel: read-only Hierarchie-Sicht, Datenquelle #420). DEV-009 bleibt konsistent (SolidJS,
  kein neuer Frontend-Stack).
- **Sonst** → reiner **Compliance-Check** (kein Edit), wie bei #427/#429.
Falls editiert: **beide Kopien sprachgetrennt** (DE-SSOT bleibt Deutsch, NIE `cp` SSOT→Repo).

## #395-Kopplung (dokumentiert)
Der Tier pro Node kommt aus `runtime.nano_runtime` und wird **read-only als Roh-String** angezeigt; bis
#395 (Tier-Schema, OPEN) ist er in den Config-Agents `null` → Anzeige `"—"` (live bestaetigt: 60/60 Agents
"—"). Nach #395 zeigt derselbe Node den dann gesetzten Tier-Wert ohne Code-Aenderung.
