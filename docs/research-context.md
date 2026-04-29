# Research Context

> The synthetic office workload that runs on top of Project Sentinel is a
> deliberate stress-test for the runtime layer. This document collects the
> personality model, role taxonomy, bio-state mechanism, and the
> ethical/research framing.
>
> **The platform underneath is the work; the workload is the evaluation.**
> If you are evaluating Sentinel as a runtime, the
> [Architecture Guide](architecture/togaf-architecture-guide.html) and the
> [Sandbox Test Report](security-test-report.md) are the primary documents.

---

## Why this workload was chosen as the stress-test

Three properties make a 60-agent persona simulation a useful stress test
for an agent runtime:

1. **Concurrent sandbox lifetimes.** ~26 agents tick simultaneously per
   shift (17 shift-bound + 9 always-on duty staff). That exercises the
   per-agent sandbox stack at realistic concurrency, not at toy scale.
2. **Heterogeneous workloads.** Different roles produce different I/O,
   memory and CPU patterns. The runtime sees a real distribution of
   behaviors instead of one synthetic benchmark.
3. **Long-lived state.** Bio-state (hunger, caffeine, fatigue, social
   need) accumulates across ticks, which forces the event store, the
   projection layer, and the night-run consolidation pipeline to handle
   real append-only volume rather than a flat dataset.

The persona narrative (industry, location, employer name) is a research
convention to keep agent prompts coherent across many ticks. It is **not**
a product claim about the runtime.

---

## Personality model — Big Five (OCEAN)

Each agent carries a five-dimensional OCEAN vector
(Openness, Conscientiousness, Extraversion, Agreeableness, Neuroticism)
in `config/agents/AGENT-*.toml`. The vector is fed to the prompt compiler
as an `agent-identity` system block at every LLM call, so role-consistent
behavior is a property of the persona, not of the prompt template.

## Role taxonomy

Eight role categories cover the persona pool:

| Role | Count |
|------|-------|
| developer       | majority of shift-bound staff |
| designer        | a smaller per-shift contingent |
| management      | head-of-X roles, one per shift |
| works council   | always-on duty (3, one per shift handover) |
| occupational psychologist | always-on duty (3) |
| occupational physician    | always-on duty (3) |
| medical         | shift-rotated nurse-equivalent |
| ops             | platform operator role |

Counts per role are derived from `config/agents/AGENT-*.toml`.

## Schedule — three shifts plus always-on duty

| Set | Hours | Count | Notes |
|-----|-------|-------|-------|
| Set 1 (early) | 06:00 – 13:59 | 17 | full role mix |
| Set 2 (mid)   | 14:00 – 21:59 | 17 | same role distribution as Set 1 |
| Set 3 (late)  | 22:00 – 05:59 | 17 | same role distribution as Set 1 |
| Set 0 (24/7)  | always-on | 9 | 3 works council + 3 occupational psychologist + 3 occupational physician |

Approximately 26 personas are active at any given moment (one shift + the
always-on duty pool).

## Bio-state mechanism

Six differential equations in `crates/sentinel-bio/` track per-agent
state across ticks: `hunger`, `energy`, `caffeine`, `bladder`, `stress`,
`social need`. The values feed into the perception block of the next LLM
call, so an agent that has not eaten for many ticks reports hunger as a
real internal sensation rather than as out-of-band metadata.

This matters for the runtime evaluation because it produces realistic
churn in event-store volume, projection lag, and synthesis-engine
intercept rate — all things a runtime operator would care about.

## Self-Recognition Pattern Detection

The Cortex Gateway includes a 15-pattern regex set plus a two-stage LLM
judge in `cmd/cortex-gateway/internal/detection/`. It fires when an agent
response surfaces awareness of being a simulation (e.g., "I am an AI", "as
an LLM"). On a hit, the response is regenerated. The detector is
documented (with the legacy "Fourth-Wall Detection" anchor preserved) in
[docs/glossary.md](glossary.md#fourth-wall-detection).

The synthesis engine sits in front of the detector and intercepts ~70% of
routine perceptions before they reach a real LLM call, which is what
keeps the per-tick LLM cost of a 60-persona simulation tractable.

---

## What this workload models

- Concurrent agent lifetimes under a real per-agent sandbox
- Long-lived event sourcing under realistic state churn
- Cross-role interaction patterns (hierarchy, role-conflict, hand-over)
- The cost of synthesis vs real LLM calls at scale

## What this workload does NOT model

- Real customer environments — the persona narrative is fiction by design
- Real coding tasks — agents do not write production code
- Performance against an SLA — no latency or throughput guarantees
- Multi-tenant company configs — single-tenant for this release

## Ethical and research framing

The persona narrative — including the fictional employer name
(**PixelPerfekt GmbH**), the industry framing, and the synthetic role
roster — is a research convention used to keep agent prompts coherent
across many ticks. **The company does not exist outside the configs.**
Any resemblance to real organizations is coincidental.

The narrative framing has been described as Truman-Show-style elsewhere:
agents are evaluated against a coherent, believable reality rather than
a stub environment. This is a research convention, not a product claim.

For detailed agent rosters and role definitions see
[docs/glossary.md](glossary.md). For the architecture context that this
workload exercises see the
[TOGAF v22.1 Architecture Guide](architecture/togaf-architecture-guide.html)
and the [per-cluster gap report](togaf-gap-v22.md).
