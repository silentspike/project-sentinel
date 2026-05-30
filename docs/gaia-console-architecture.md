# Gaia-Konsole — Architektur-Entscheidungen (SSOT)

> Single Source of Truth fuer die Gaia-Konsole: das User-Interface zu Project Sentinel.
> Alle Issues zu diesem Vorhaben verweisen hierher. Das **Was/Wie** steht im jeweiligen Issue,
> das **Warum** hier. Status: Entscheidungen final (Maintainer, 2026-05-30), Umsetzung folgt.
>
> Querverweise: `docs/togaf-deviations-v22.md` (DEV-008/DEV-009), `/work/company/AUDIT-sentinel-pp-noaide.md`,
> Memory `decision_gaia_is_claude_code.md`. Design-Polish ist bewusst ein **eigenes spaeteres Thema** (vor Go-Live).

---

## 0. Leitprinzip — Polyglot, best of all worlds

Sentinel ist bewusst **polyglot** (Rust + Go + …). Ziel ist nicht Einheitssprache, sondern
**Overhead-Reduktion, I/O-Reduktion, Performance-Maximierung** — jede Schicht bekommt ihr
overhead-aermstes Werkzeug. (Lauffaehigkeit auf schwacher Hardware ist der Nebeneffekt davon,
nicht das Ziel; primaer geht es um Overhead/IO/Perf auf moeglichst starker Hardware.)
Dieses Prinzip praegt jede Tech-Entscheidung unten.

---

## 1. Vision & Rolle von Gaia

**Gaia ist die durchgaengige, reaktive User-Schnittstelle zu Sentinel** — eine vollwertige
**Claude-Code-Instanz**, die *ueber* der bereits autonomen Firma sitzt. Drei Rollen:

1. **Setup** — Firma im Dialog erstellen (adaptiv-dialogisches Interview).
2. **Orchestrierung (nur auf Auftrag)** — „mach einen Plan und kuemmere dich um die Umsetzung" →
   Gaia plant, delegiert hierarchisch, ueberwacht (Task-Entity + Voice-of-Gaia).
3. **Observability/Kontrolle** — Voll-Sicht + Steuerung von Firma *und* Plattform.

**Gaia handelt nie von sich aus.** Die Firma reguliert/heilt/lernt/verbessert sich bereits selbst
(siehe §6). Gaia ist rein reaktiv: keine eigene Agenda, keine laufenden Tasks ausser auf
expliziten User-Auftrag. Der „Bereitschafts-Loop" (§3) informiert den User, greift nicht selbst ein.

### Drei verschiedene „Gaia" — nie verwechseln
| Begriff | Was es ist |
|---|---|
| **Gaia** (Interface) | Claude-Code-Instanz, das User-Tor. Reaktiv. Ruft Werkzeuge auf. |
| **`sentinel-gaia`** (#414–416) | Deterministisches Generator-Tool (Rust, blake3, kein LLM), das Gaia *aufruft*. |
| **Voice of Gaia** | Laufzeit-Gedankeninfusion an Sim-Agents (`OperatorGaiaCommand` → `inner-voice`-`<system>`-Block, getarnt als agenten-eigener Gedanke). **Bereits integriert** (`cortex-gateway/.../structured.go`). |

---

## 2. Werkzeug-Zugriff — CLI statt MCP

**Entscheidung:** Kein MCP-Server. Stattdessen ein **`sentinel-ctl`-CLI** (Rust), das Gaia via
**Bash** aufruft (Claude Code nativ). Gaia laeuft `claude -p` **lokal auf derselben VM** wie das
Backend → ein MCP-Server (eigener Prozess + HTTP/SSE-Transport + Protokoll-Roundtrip) waere reiner
Overhead; er lohnt nur remote/multi-client.

- Kapselt Operator-API + Telemetrie + Events + Platform-Admin als **feinkoernige Subcommands**
  (`chat-to-room`, `set-agent-tier`, `apply-config`, `restore`, `platform …`).
- Jeder mutierende/hochriskante Subcommand laeuft durch **Policy-as-Code (#391)** + Konsolen-Gate.
- Bonus: dasselbe CLI ist direkt vom User nutzbar + deterministisch testbar (kein Server-Mock).

---

## 3. Laufzeit-Modell

- **Hybrid**: leichter, event-/schedule-getriggerter **Bereitschafts-Loop** (informiert/benachrichtigt
  den User, handelt nicht selbst) + **tiefe Sessions on-demand** bei Auftrag.
- **Spawn**: pro Auftrag eine **headless `claude -p`**-Session (Subscription, claude-code-Provider-Pfad),
  `--resume` fuer Mehrturn-Kontext, beendet nach Erledigung. Token-bewusst.
- **Token**: nur Monitoring, kein hartes Limit; Live-Cost-Sicht via API-Cost-Control-Plane + OTel-GenAI (#427).

---

## 4. Frontend — Polyglot pro Schicht

Kein „eine Sprache". Jede Schicht ihr overhead-aermstes Werkzeug:

| Schicht | Werkzeug | Warum |
|---|---|---|
| **DOM / UI-Reaktivitaet** | **JS / SolidJS** (fine-grained Signals) | overhead-minimal am DOM; Rust/WASM-UI (Leptos/Dioxus) zahlt pro DOM-Update WASM↔JS-Bridge-Overhead → gegen das Ziel |
| **Heavy-Data** (CAS-Decode, Dedup, msgpack/zstd, Validierung) | **Rust → WASM** (Worker, off-main-thread) | low-level, kein GC, SIMD |
| **Rendering** (Floorplan, Live-Charts) | **WebGL/Canvas** (live) + **SVG** (Struktur) | GPU/Canvas-Performance bei vielen Datenpunkten |

- noaide/PixelPerfekt = **UI-Pattern-Referenzen** (Chat-Layout, Tool-Cards, Mobile, Kanban), **kein Code-Port**.
- **Layout/IA**: dynamisch-kachelbares Workspace-Layout im **niri/Hyprland-Stil** (freies Resizing,
  smooth Web-Animations), eigene leichte **Tiling-Engine** (SolidJS-Signals + CSS Grid + ResizeObserver).
  Drei Saeulen: **Dashboard** (Highlight, Infografiken) · **Control-Center** (Agents/Raeume/Voice-of-Gaia) · **Chat**.
- **Mobile**: native-app-artig (BottomTabBar + SwipeView + Pull-to-refresh), Desktop/Mobile via Breakpoint.
- **i18n**: Deutsch primaer, UI-Strings i18n-faehig (keine Hardcodes). Gaia-Dialog ist als LLM ohnehin multilingual.
- **Design-Polish**: bewusst **vertagt** als eigenes grosses Thema (vor Go-Live, parallel zu Abnahmetests).
  Jetzt nur **funktionales** Design; die Layout-Architektur (Tiling) entsteht aber schon jetzt.

---

## 5. Daten — CAS-Konsolen-Datenebene (1:n-Pointer)

Das heutige Dashboard pollt 1s + sendet Voll-State (laggy). Loesung = Sentinels eigenes
**1:n-Pointer/CAS-Prinzip** (`sentinel-fs`: content-defined chunking + blake3 + refcount-Dedup + zstd,
99,2 % erprobt) auf den Konsolen-Datenstrom:

- **Eigene Console-Data-Plane**, die dieselben `sentinel-fs`-Primitive nutzt, aber auf **Stream/Append** optimiert.
- **Wire**: Push (WebTransport/QUIC) statt Poll; **Client-Manifest + Server-Delta** — Client zieht nur
  Bloecke, die er noch nicht hat (Conversations/System-Bloecke sind massiv redundant → Dedup greift stark).
- **Client-Store**: **OPFS** fuer Binaer-Bloecke + **IndexedDB-Fallback**, hinter einem Interface.
- **Observability-Tiefe**: aggregierte Live-Views default, **Drill-Down on-demand** (rohe Events/Internals
  lazy via CAS).
- **Visualisierung**: Live-**Floorplan (2D)** + Daten-Charts.

---

## 6. Self-* Systeme — Gaia dockt an, ersetzt NICHTS

**Audit-Ergebnis (verifiziert, integriert UND aktiv):** Die Firma reguliert/heilt/lernt/verbessert sich
bereits selbst. Gaia ist die strategische/User-Schicht *darueber*, nicht ein konkurrierender Controller.

- **Self-Healing**: Agent-Control-Plane (`controlplane/` observe/decide/act/verify, TTL+rollback),
  **Platform-CP** (`platform_controlplane/` Stall/EventStore/ProjectionLag/MemoryPressure) → bei
  Fehlschlag **`llm_analyzer`-Eskalation** (aktiv: `enqueue`, orchestrator:2812/3016) → **`escalate_to_operator`-Pfad** (Z.2085). Circuit-Breaker.
- **Self-Improving**: **Adaptive Tick** (PSI-Throttling), API-CP (Kosten via Synthesis).
- **Self-Learning**: **Hippocampus/NMDA** (Episode→Sleep-Cycle-Konsolidierung→Narrative), **Nightrun**
  (Evolution ohne Modelltraining), **Judge** (Drift/Quality/Fatigue/Swap), `evolution_task`, `EpisodeProducer`.

### Leitplanken (jede Gaia-Integration MUSS sie respektieren)
1. **An `escalate_to_operator` andocken** — Gaia ist der Operator, empfaengt Eskalationen, macht sie
   sichtbar/beraet. KEIN zweiter Healing-LLM-Loop, `llm_analyzer` bleibt.
2. **Personality/Evolution nicht direkt ueberschreiben** (TOML = unveraenderliche Identitaet; Evolution
   ist autonom).
3. **Keine Kollision mit CP-Actions** (Vorrang/Koordination — nicht den Agent despawnen, den die CP heilt).
4. **Adaptive-Tick/Resource-Manager nicht uebersteuern**.
5. **Task-Entity koexistiert** mit der emergenten Agent-Autonomie, ersetzt sie nicht.

---

## 7. Memory

**Kein semantisches Embedding** (zu viel Last/Overhead). Stattdessen auf Vorhandenem aufbauen:

- **Agent-Memory** = bereits vollstaendig: Events (Limbo) → `EpisodeProducer` (alle 30s) → Episode →
  **Nightrun-Konsolidierung** (NMDA-Scoring + Narrative-Building) → Archive. JSONL/Outputs liegen im
  virtuellen FS (CAS). Per Agent getrennt (gewollt). Bleibt; ggf. spaeter optionaler Recall.
- **Gaia-Memory** = **Event-Rehydration + Gaia-Memory-File** (Setup, offene Tasks, Praeferenzen) **plus**
  ein **eigener Rust-Graph (relational-temporal, OHNE Vektor)** fuer Gaias Wissen ueber Firma/User/Entscheidungen
  (embedded auf redb/Limbo, SOTA-Prinzipien: bi-temporal, Staleness-bewusst — Graphiti-Idee, kein Fremd-Service).
- **Semantische Abfrage** macht **Gaia als LLM** selbst (liest verdichtete Narratives + zieht Rohdaten on-demand).
- Persistenz in die bestehenden Sentinel-Backups eingebunden.

---

## 8. Weitere Festlegungen

- **Arbeitsmodell**: **Task-Entity** (ECS, event-sourced, hierarchisch, Status pending→in_progress→done/blocked,
  Kanban-Backing) **+ Voice-of-Gaia** als In-Sim-Zustellweg. Fortschritt aus Agent-Aktionen, Gaia ueberwacht via Projection.
- **Chat-Scope**: volle PixelPerfekt-Paritaet — Room-Chat (existiert) + **1:1-Agent-DM** (neu) + **Room-Invite** (neu) + reiche Chat-UI.
- **company-context**: Gaia (Claude Code) generiert `company-context.md` aus dem Interview; **Gateway-Hot-Reload-Endpoint** macht Aenderungen live wirksam (Gateway cacht heute statisch).
- **Soziale Dimension**: strukturierte Kultur-/Sozial-Felder in der `gaia-spec` (steuern Big-Five-Verteilung deterministisch) **+** Prosa im company-context.
- **Gaia-Transparenz**: umschaltbar — Ergebnis-Sicht default, **Deep/Supervision-Modus** zeigt Gaias JSONL/Tool-Stream (noaide-Pattern) mit Gates.
- **Auth**: Server-Session + httpOnly-Cookie (#405-Muster), Desktop+Mobile.
- **Gaia-Persona**: neutrale Assistenz (kein Rollenspiel) + dynamisches Firmen-Wissen (Backend-injiziert) + CLI-Tools.
- **Editier-Modalitaet**: beides — Gaia-Dialog **und** strukturierte UI-Editoren.
- **Setup-Interview**: adaptiv-dialogisch mit interner Vollstaendigkeits-Checkliste.
- **Time-Travel**: voll — bewusster Total-Restore (gegatet) **+** Gaia-gesteuerte selektive Extraktion (konversationell, ohne Welt-Reset).
- **Platform/Nano-Container-Admin**: voll — observe + verwalten + Gaia-orchestriert (ueber CLI, mit Gates).
- **Firmen-Scope**: **single aktiv + Firmen-Bibliothek** (gaia-specs speichern/laden/umschalten via #425 Fresh-Load); kein Multi-Tenant.
- **Benachrichtigung**: nur In-Konsole-Alerts (kein ntfy/Web-Push).
- **Deployment**: alles auf Deploy-VM (10.0.0.240) als systemd-Services, nginx :8000.
- **Test-Strategie**: mehrschichtig — Unit + Integration + Playwright-E2E + Gaia-Eval.
- **Dashboard-Migration**: schrittweise abloesen, bestehendes Hono+Vanilla-Dashboard bleibt bis Feature-Paritaet, dann Cut-over.

---

## 9. Bestandsaufnahme — existiert / nur Anbindung / Neubau

**Existiert bereits (verifiziert):** Room-Chat (`RoomChatBuffer`), Voice-of-Gaia (`inner-voice`-`<system>`),
Operator-API (chat/broadcast/restore/snapshot/nightrun/gaia/platform-analysis), Self-*-Systeme (CP, Platform-CP +
`llm_analyzer` + `escalate_to_operator`, Judge, Nightrun, Hippocampus, `evolution_task`, `EpisodeProducer`),
Tages-Memory-Verdichtung, `sentinel-fs` CAS, Time-Machine (Snapshot/Restore/Replay), Telemetrie, Projection,
Floorplan-View, `sentinel-gaia` Generator (#414–416), Agent-`[identity]` (role/department/KPIs/Hierarchie),
`company-context.md` (PixelPerfekt).

**Nur Anbindung (Backend da, UI/Trigger fehlt):** Cost/Token-Tracking (API-CP existiert, `apicp_enabled=false`),
company-context-Hot-Reload (Datei da, Reload fehlt), Observability-Views auf vorhandene Telemetrie/Events,
Platform-CP-State sichtbar/steuerbar.

**Neubau:** `sentinel-ctl`-CLI, Konsolen-Frontend (Polyglot-Stack), CAS-Konsolen-Datenebene + Push,
Task-Entity (ECS+Events), Gaia-Claude-Instanz + Bereitschafts-Loop + Setup-Interview, 1:1-Agent-DM, Room-Invite,
soziale `gaia-spec`-Felder + company-context-Generierung durch Gaia, Gaia-Memory-Graph, Tiling-Engine,
selektive Time-Travel-Extraktion.

---

## 10. Roadmap (Bau-Reihenfolge)

Backend-first — nie tote UI-Platzhalter (Sentinel-Kultur).

1. **Phase 1 — Backend-Fundament**: CAS-Konsolen-Datenebene (auf #431), `sentinel-ctl`-CLI, Task-Entity,
   Config-Apply (#425), Gateway-Hot-Reload, soziale `gaia-spec`-Felder + company-context-Generierung,
   API-CP aktivieren (Cost, #427).
2. **Phase 2 — Konsole-Shell**: SolidJS-Shell + Tiling-Engine + Auth (#405-Muster) + WebTransport-Push +
   Floorplan/Chat-Views + Mobile-Layout.
3. **Phase 3 — Gaia**: `claude -p`-Bereitschafts-Loop, Setup-Interview, Voice-of-Gaia-Delegation, Gaia-Memory-Graph,
   Deep/Supervision-Modus, Platform-Admin.
4. **Phase 4 — reiche Features**: 1:1-Agent-DM + Invite, selektive Time-Travel-Extraktion, Cost-Deep,
   Org-Chart, Editoren, Firmen-Bibliothek.
5. **Quer**: TOGAF-HTML-Aktualisierung (Gaia-Konsole als Komponente) — eigenes Issue.
6. **Vor Go-Live**: Design-/Aesthetics-Phase (eigenes grosses Thema) + Abnahmetests.

Bestehende Epics einordnen: **#418** (Configure/Build), **#426** (Observe/Govern), **#430** (SOTA-Stack)
werden Teil dieser Phasen — aktualisieren statt duplizieren.
