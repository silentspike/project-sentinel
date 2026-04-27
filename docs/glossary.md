# Glossary

This document explains internal terms and narrative conventions used throughout
the Sentinel codebase.

## PixelPerfekt GmbH

Fictional web design agency based in Nuernberg, which serves as the narrative
framing of the simulation. Every LLM-persona agent believes they are an
employee of PixelPerfekt GmbH, located at Fuerther Strasse 42, 90429 Nuernberg.
Founded (in-story) 2019, ~1.2M EUR annual revenue, 60 employees across shifts.

**This company does not exist in reality.** The address, revenue, employee
roster, and organizational structure are synthetic data for Truman-Show-style
agent-belief experiments. Any resemblance to real organizations at this address
is coincidental.

The choice to embed fictional details deep in configs
(`config/company-context.md`, `config/agents/AGENT-*.toml`) rather than using
placeholder names reflects the core experiment: agents are evaluated against a
coherent, believable reality, not a stub environment.

## Agent Layers

Two distinct agent categories coexist in this project. They share the word
"agent" but operate at completely different abstraction levels.

### LLM-persona agents (60)

Human-role simulations with personality, bio-state, memory, and shift-bound
schedules. Each is one row in `config/agents/AGENT-*.toml` with a Big Five
personality vector, a role (developer, designer, management, works council,
medical, occupational psychology), and a shift assignment.

| Set | Hours | Count | Notes |
|-----|-------|-------|-------|
| Set 1 (early) | 06:00-13:59 | 17 | Developers, designers, QA, delivery, CEO, CTO, etc. |
| Set 2 (mid)   | 14:00-21:59 | 17 | Same role distribution as Set 1 |
| Set 3 (late)  | 22:00-05:59 | 17 | Same role distribution as Set 1 |
| Set 0 (24/7)  | always-on   |  9 | 3 works council + 3 occupational physicians + 3 occupational psychologists |

Approximately 26 LLM-persona agents are active at any given simulated moment
(17 from the current shift + 9 always-on duty staff).

### Background service agents (5)

Rust and Go services running the platform itself. They are agents in the
"autonomous-process-with-control-loop" sense, not the "human-roleplay" sense.

| Service | Language | Role |
|---------|----------|------|
| `sentinel-daemon`     | Rust | ECS world, tick loop, event sourcing, persistence, agent runtime |
| `cortex-gateway`      | Go   | LLM proxy with synthesis engine, controlplane, MITM forward |
| `sentinel-judge`      | Go   | Quality + drift monitoring (NATS streaming + LLM analysis) |
| `sentinel-nightrun`   | Rust | Nightly batch consolidation, deterministic replay, hash chain |
| `sentinel-nats-bridge`| Go   | eBPF metrics dual-publish (Limbo to NATS) |

Both layers run under sandbox isolation (bwrap + Landlock + cgroups v2 + netns
where applicable). The simulated office is the **evaluation context** for
stress-testing runtime hardening primitives, agent control loops, and
boundary detection.

## TOGAF

The Open Group Architecture Framework. This project uses **TOGAF v22.1** as
its authoritative architecture reference, located at
[docs/architecture/togaf-architecture-guide.html](architecture/togaf-architecture-guide.html).

The architecture is structured into **12 clusters**. Per-cluster implementation
status and deviation register live in
[docs/togaf-gap-v22.md](togaf-gap-v22.md) and
[docs/togaf-deviations-v22.md](togaf-deviations-v22.md).

## Controlplane Kernel

Native in-process observe / decide / act / verify loop inside `sentinel-daemon`.
Every auto-action carries a rollback condition and a TTL. See
[docs/governance.md](governance.md) and TOGAF cluster 05b for details.

## Synthesis Engine

Located in `cmd/cortex-gateway/internal/synthesis/`. Ten deterministic rules
intercept routine perceptions before they are forwarded to a real LLM. Goal:
reduce LLM call volume for trivial state-changes (~70% intercept rate in
typical workloads). The base gate is provider-independent and triggers on
`!HasHeard && !IsAddressed && !HasChaos && !HasImpulse`.

## Perception Injection

Pattern used to give an LLM-persona agent its world-state for a single tick.
The Cortex Gateway prepends `[SYSTEM_INJECTION] ... [/SYSTEM_INJECTION]`
blocks to the agent's prompt with body-state, environment cues, and social
perception. The agent treats these as in-character sensations, not as
metadata.

## Self-Recognition Pattern Detection

<a id="fourth-wall-detection"></a>

(Also referred to in older docs and code paths as "Fourth-Wall Detection";
the anchor `#fourth-wall-detection` is preserved for back-compatibility.)

Fifteen regex patterns plus a two-stage LLM-judge in
`cmd/cortex-gateway/internal/detection/`. Fires when an agent response shows
awareness of being a simulation (e.g., "I am an AI", "as an LLM"). On hit the
response is regenerated.

## NMDA Night-Run

Shift-change memory consolidation pipeline in `services/sentinel-nightrun/`.
Six-phase finite state machine: Awake -> Collecting -> Scoring -> Selecting ->
Consolidating -> WakingUp. **No model training**: this is episodic-memory
selection and compression, not weight updates.

## Voice of Gaia

Highest-priority internal perception channel. Used for system-level prompts
that override an agent's normal decision flow. Implemented as the
`inner-voice` system block (one of eight system blocks) in the Cortex Gateway
prompt compiler.

## Diegetic Hardware Mapping

Convention where infrastructure events are surfaced to agents as in-character
sensations rather than as out-of-band signals.

| Hardware event   | Agent perception          |
|------------------|---------------------------|
| CPU throttle     | afternoon fatigue         |
| API timeout      | migraine                  |
| RAM pressure     | trouble concentrating     |
| Network latency  | "having a slow day"       |
