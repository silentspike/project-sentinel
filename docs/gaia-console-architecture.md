# Gaia Console - Architecture Decisions (SSOT)

> Single Source of Truth for the Gaia Console: the user interface for Project Sentinel.
> All issues for this initiative point here. The **what/how** is in the respective issue,
> the **why** is here. Status: decisions final (Maintainer, 2026-05-30), implementation follows.
>
> Cross-references: `docs/togaf-deviations-v22.md` (DEV-008/DEV-009), `/work/company/AUDIT-sentinel-pp-noaide.md`,
> Memory `decision_gaia_is_claude_code.md`. Design polish is deliberately a **separate later topic** (before go-live).

---

## 0. Guiding Principle - Polyglot, best of all worlds

Sentinel is deliberately **polyglot** (Rust + Go + ...). The goal is not one unified language, but
**overhead reduction, I/O reduction, performance maximization** - every layer gets its
lowest-overhead tool. (Ability to run on weak hardware is the side effect of that,
not the goal; the primary focus is overhead/IO/perf on the strongest possible hardware.)
This principle shapes every technical decision below.

---

## 1. Vision & Role of Gaia

**Gaia is the continuous, reactive user interface to Sentinel** - a full-fledged
**Claude Code instance** that sits *above* the already autonomous company. Three roles:

1. **Setup** - create the company through dialog (adaptive dialog interview).
2. **Orchestration (only when instructed)** - "make a plan and take care of implementation" ->
   Gaia plans, delegates hierarchically, monitors (Task Entity + Voice of Gaia).
3. **Observability/control** - full visibility + control of company *and* platform.

**Gaia never acts on its own.** The company already regulates/heals/learns/improves itself
(see section 6). Gaia is purely reactive: no agenda of its own, no running tasks except on
explicit user instruction. The "standby loop" (section 3) informs the user; it does not intervene on its own.

### Three Different "Gaia" - Never Confuse Them
| Term | What it is |
|---|---|
| **Gaia** (Interface) | Claude Code instance, the user gateway. Reactive. Calls tools. |
| **`sentinel-gaia`** (#414-416) | Deterministic generator tool (Rust, blake3, no LLM) that Gaia *calls*. |
| **Voice of Gaia** | Runtime thought infusion into Sim agents (`OperatorGaiaCommand` -> `inner-voice`-`<system>` block, disguised as an agent's own thought). **Already integrated** (`cortex-gateway/.../structured.go`). |

---

## 2. Tool Access - CLI Instead of MCP

**Decision:** No MCP server. Instead, a **`sentinel-ctl` CLI** (Rust) that Gaia invokes via
**Bash** (Claude Code native). Gaia runs `claude -p` **locally on the same VM** as the
backend -> an MCP server (own process + HTTP/SSE transport + protocol roundtrip) would be pure
overhead; it only pays off for remote/multi-client setups.

- Encapsulates Operator API + telemetry + events + Platform Admin as **fine-grained subcommands**
  (`chat-to-room`, `set-agent-tier`, `apply-config`, `restore`, `platform …`).
- Every mutating/high-risk subcommand goes through an **operator-side policy gate** (RiskLevel Read/Mutate/HighRisk + confirmation) + console gate. **NOT** #391 - that is the gateway-side *agent* tool policy (`agent_policy.go`); the ctl gate is the independent operator layer.
- Bonus: the same CLI is directly usable by the user + deterministically testable (no server mock).

---

## 3. Runtime Model

- **Hybrid**: lightweight, event-/schedule-triggered **standby loop** (informs/notifies
  the user, does not act on its own) + **deep sessions on demand** when instructed.
- **Spawn**: one **headless `claude -p`** session per task (subscription, claude-code provider path),
  `--resume` for multi-turn context, exits after completion. Token-aware.
- **Token**: monitoring only, no hard limit; live cost view via API Cost Control Plane + OTel GenAI (#427).

---

## 4. Frontend - Polyglot Per Layer

No "one language". Every layer gets its lowest-overhead tool:

| Layer | Tool | Why |
|---|---|---|
| **DOM / UI reactivity** | **JS / SolidJS** (fine-grained Signals) | minimal DOM overhead; Rust/WASM UI (Leptos/Dioxus) pays WASM<->JS bridge overhead per DOM update -> against the goal |
| **Heavy Data** (CAS decode, dedup, msgpack/zstd, validation) | **Rust -> WASM** (Worker, off-main-thread) | low-level, no GC, SIMD |
| **Rendering** (floorplan, live charts) | **WebGL/Canvas** (live) + **SVG** (structure) | GPU/Canvas performance with many data points |

- noaide/PixelPerfekt = **UI pattern references** (chat layout, tool cards, mobile, Kanban), **no code port**.
- **Layout/IA**: dynamically tileable workspace layout in the **niri/Hyprland style** (free resizing,
  smooth web animations), custom lightweight **tiling engine** (SolidJS Signals + CSS Grid + ResizeObserver).
  Three columns: **Dashboard** (highlight, infographics) · **Control Center** (agents/rooms/Voice of Gaia) · **Chat**.
- **Mobile**: native-app-like (BottomTabBar + SwipeView + pull-to-refresh), desktop/mobile via breakpoint.
- **i18n**: German primary, UI strings i18n-capable (no hardcodes). Gaia dialog is multilingual as an LLM anyway.
- **Design polish**: deliberately **deferred** as a separate large topic (before go-live, in parallel with acceptance tests).
  For now only **functional** design; the layout architecture (tiling) is already created now.

---

## 5. Data - CAS Console Data Plane (1:n Pointer)

Today's dashboard polls every 1s + sends full state (laggy). Solution = Sentinel's own
**1:n pointer/CAS principle** (`sentinel-fs`: content-defined chunking + blake3 + refcount dedup + zstd,
99.2% proven) applied to the console data stream:

- **Dedicated Console Data Plane** that uses the same `sentinel-fs` primitives, but optimized for **stream/append**.
- **Wire**: Push (**WebTransport/QUIC only** - Maintainer 2026-05-31: NO WebSocket fallback; WebTransport has been Baseline since March 2026, no duplicate transport for legacy browsers); **Client Manifest + Server Delta** - the client fetches only
  blocks it does not already have (Conversations/System blocks are massively redundant -> dedup is very effective).
- **Client Store**: **OPFS** for binary blocks + **IndexedDB fallback**, behind an interface.
- **Observability depth**: aggregated live views by default, **drill-down on demand** (raw events/internals
  lazy via CAS).
- **Visualization**: live **floorplan (2D)** + data charts.

---

## 6. Self-* Systems - Gaia Docks On, Replaces NOTHING

**Audit result (verified, integrated AND active):** The company already regulates/heals/learns/improves
itself. Gaia is the strategic/user layer *above* that, not a competing controller.

- **Self-Healing**: Agent Control Plane (`controlplane/` observe/decide/act/verify, TTL+rollback),
  **Platform CP** (`platform_controlplane/` Stall/EventStore/ProjectionLag/MemoryPressure) -> on
  failure **`llm_analyzer` escalation** (active: `enqueue`, orchestrator:2812/3016) -> **`escalate_to_operator` path** (line 2085). Circuit breaker.
- **Self-Improving**: **Adaptive Tick** (PSI throttling), API CP (costs via Synthesis).
- **Self-Learning**: **Hippocampus/NMDA** (episode -> sleep-cycle consolidation -> narrative), **Nightrun**
  (evolution without model training), **Judge** (drift/quality/fatigue/swap), `evolution_task`, `EpisodeProducer`.

### Guardrails (Every Gaia Integration MUST Respect Them)
1. **Dock onto `escalate_to_operator`** - Gaia is the operator, receives escalations, makes them
   visible/advises. NO second healing LLM loop; `llm_analyzer` remains.
2. **Do not autonomously/secretly overwrite the evolution layer.** The *autonomous* evolution (redb: Voice-Style/
   Behavioral-Notes, from Nightrun) is additive and does NOT change the base TOML - "TOML = immutable
   identity" means exactly that (evolution leaves the base alone), NOT that the user is not allowed to change it.
   **An explicit user edit of the base personality is legitimate and takes effect LIVE** on the running agent - NO
   despawn/respawn (that would destroy memory + evolution): `apply_personality` (live component update,
   world.rs:1365) + TOML persistence + **Gateway DNA cache invalidation** (the `TOMLLoader` caches the Big Five
   per agent -> the edit reaches the prompt only after reload; extension of #440 to agent DNA). Memory + evolution
   remain intact. Structural fields (role/tier/caps) are live anyway. Despawn only for *removed* agents.
3. **No collision with CP actions** (priority/coordination - do not despawn the agent the CP is healing).
4. **Do not override Adaptive Tick/Resource Manager**.
5. **Task Entity coexists** with emergent agent autonomy; it does not replace it.

---

## 7. Memory

**No semantic embedding** (too much load/overhead). Instead, build on what already exists:

- **Agent Memory** = already complete: events (Limbo) -> `EpisodeProducer` (every 30s) -> episode ->
  **Nightrun consolidation** (NMDA scoring + narrative building) -> archive. JSONL/outputs are in the
  virtual FS (CAS). Separated per agent (intentional). Stays; optional recall maybe later.
- **Gaia Memory** = **event rehydration + Gaia memory file** (setup, open tasks, preferences) **plus**
  a **dedicated Rust graph (relational-temporal, WITHOUT vector)** for Gaia's knowledge about company/user/decisions
  (embedded on redb/Limbo, SOTA principles: bi-temporal, staleness-aware - Graphiti idea, no third-party service).
- **Semantic query** is done by **Gaia as LLM** itself (reads condensed narratives + fetches raw data on demand).
- Persistence integrated into existing Sentinel backups.

---

## 8. Further Decisions

- **Work model**: **Task Entity** (ECS, event-sourced, hierarchical, status pending->in_progress->done/blocked,
  Kanban backing) **+ Voice of Gaia** as the in-simulation delivery path. Progress comes from agent actions; Gaia monitors via Projection.
- **Chat scope**: full PixelPerfekt parity - room chat (exists) + **1:1 agent DM** (new) + **room invite** (new) + rich chat UI.
- **company-context**: clear separation of content/form - **Gaia (LLM) supplies the content** by filling the structured `gaia-spec` fields (`mission`, `values[]`, culture/social axes) during the interview; **`sentinel-gaia` renders from that deterministically** (template, no LLM, blake3-reproducible) into the complete `company-context.md` (mission/values + org chart prose from departments/hierarchy/KPIs). This makes every generated company immediately standalone - even a pure CLI company without a Gaia run (no PixelPerfekt default, AC-3). **Optional narrative LLM enrichment** of the prose happens later (Gaia loop, not a prerequisite). **Gateway hot-reload endpoint** (#440) makes changes live (Gateway caches company-context + agent DNA statically).
- **Social dimension**: structured culture/social fields in the `gaia-spec` (control the Big Five distribution deterministically) **+** flow into the deterministically rendered company-context (no passthrough of free LLM prose).
- **Gaia transparency**: toggleable - result view by default, **Deep/Supervision mode** shows Gaia's JSONL/tool stream (noaide pattern) with gates.
- **Auth**: server session + httpOnly cookie (#405 pattern), desktop+mobile.
- **Gaia persona**: neutral assistant (no roleplay) + dynamic company knowledge (backend-injected) + CLI tools.
- **Edit modality**: both - Gaia dialog **and** structured UI editors.
- **Setup interview**: adaptive dialog with internal completeness checklist.
- **Time Travel**: full - deliberate total restore (gated) **+** Gaia-controlled selective extraction (conversational, without world reset).
- **Platform/Nano Container Admin**: full - observe + manage + Gaia-orchestrated (via CLI, with gates).
- **Company scope**: **single active + company library** (save/load/switch gaia-specs via #425 Fresh Load); no multi-tenant.
- **Notification**: in-console alerts only (no ntfy/Web Push).
- **Deployment**: everything on the deploy VM (10.0.0.240) as systemd services; console via `sentinel-dashboard-backend` on HTTPS/WebTransport `:8001`.
- **Test strategy**: layered - unit + integration + Playwright E2E + Gaia eval.
- **Dashboard migration**: #433 migrates the nine views strictly into the SolidJS console; the Bun/Hono path is removed after view parity.

---

## 9. Inventory - Exists / Only Integration / New Development

**Already exists (verified):** room chat (`RoomChatBuffer`), Voice of Gaia (`inner-voice`-`<system>`),
Operator API (chat/broadcast/restore/snapshot/nightrun/gaia/platform-analysis), Self-* systems (CP, Platform CP +
`llm_analyzer` + `escalate_to_operator`, Judge, Nightrun, Hippocampus, `evolution_task`, `EpisodeProducer`),
daily memory condensation, `sentinel-fs` CAS, Time Machine (snapshot/restore/replay), telemetry, Projection,
floorplan view, `sentinel-gaia` generator (#414-416), agent `[identity]` (role/department/KPIs/hierarchy),
`company-context.md` (PixelPerfekt).

**Only integration needed (backend exists, UI/trigger missing):** cost/token tracking (API CP exists, `apicp_enabled=false`),
company-context hot reload (file exists, reload missing), observability views on existing telemetry/events,
Platform CP state visible/controllable.

**New development:** `sentinel-ctl` CLI, console frontend (polyglot stack), CAS console data plane + push,
Task Entity (ECS+events), Gaia Claude instance + standby loop + setup interview, 1:1 agent DM, room invite,
social `gaia-spec` fields + company-context generation by Gaia, Gaia memory graph, tiling engine,
selective Time Travel extraction.

---

## 10. Roadmap (Build Order)

Backend first - never dead UI placeholders (Sentinel culture).

1. **Phase 1 - Backend foundation**: CAS console data plane (on #431), `sentinel-ctl` CLI, Task Entity,
   Config Apply (#425), Gateway hot reload, social `gaia-spec` fields + company-context generation,
   enable API CP (cost, #427).
2. **Phase 2 - Console shell**: SolidJS shell + tiling engine + auth (#405 pattern) + WebTransport push +
   floorplan/chat views + mobile layout.
3. **Phase 3 - Gaia**: `claude -p` standby loop, setup interview, Voice of Gaia delegation, Gaia memory graph,
   Deep/Supervision mode, Platform Admin.
4. **Phase 4 - rich features**: 1:1 agent DM + invite, selective Time Travel extraction, cost deep dive,
   org chart, editors, company library.
5. **Cross-cutting**: TOGAF HTML update (Gaia Console as component) - own issue.
6. **Before go-live**: design/aesthetics phase (own large topic) + acceptance tests.

Classify existing epics: **#418** (Configure/Build), **#426** (Observe/Govern), **#430** (SOTA Stack)
become part of these phases - update instead of duplicating.
