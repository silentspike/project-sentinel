# Collaboration Admission

Project Sentinel treats collaboration as a bounded execution decision, not as
the default behavior of a virtual company. One accountable employee owns each
work item. The admission policy keeps that employee in `Solo` mode when the
employee can complete routine, reversible work within the accepted authority,
capability, privacy, resource, and evidence contracts.

Additional employees are admitted only when the smallest eligible team closes
a concrete capability gap, satisfies a separation-of-duty rule, supplies an
independent evidence channel, or reduces a declared material risk. A model may
suggest a team, but it cannot decide membership, reserve capacity, grant
authority, or relax a hard constraint.

## Authority Boundary

The public admission request identifies the exact project and work item and
supplies only an explanatory benefit reference. It does not carry risk,
reversibility, ambiguity, uncertainty, separation, privacy, packet, budget,
candidate, or authority decisions. Those values are server-owned so a model,
manager, or temporary team cannot classify its own work into an easier mode.

The daemon derives the candidate snapshot from authoritative product state:

- the accepted project participant roster and permanent roles;
- the active work assignment and its exact ID, version, and digest;
- the organization and project behavior-policy generations;
- authenticated principal bindings;
- runtime and tool availability;
- current assignment load and committed collaboration reservations;
- task-local, independently verified reliability observations.

The accepted work contract also determines the policy class. Routine work
remains solo whenever its owner covers every required specialty, even if the
contract names several specialties. One foreign dependency owner or one real
capability gap requires a directed handoff. Two or more complementary helpers
or foreign dependency owners require a bounded specialist panel. High-risk,
ambiguous, uncertain, or conflicting work requires an independent QA or
Release channel. Release and Gaia effects require human authority. The
daemon derives a fixed five-minute decision window, four-round ceiling,
32,000-token ceiling, four-participant ceiling, project-local cost ceiling,
packet classes, and quality tolerance; callers cannot widen or weaken them.

The workflow store repeats these checks inside the same immediate SQLite
transaction that records the decision and reservations. A caller therefore
cannot select a preferred roster, race a stale load snapshot, or dispatch work
under an earlier organization, assignment, policy, or collaboration
generation.
The transaction reserves the complete declared admission cost ceiling, divided
deterministically across the selected participants, rather than trusting an
optimistic per-agent estimate. A terminal outcome releases that ceiling.

## Admission Modes

`Solo` is the normal mode. It has one accountable owner and no collaboration
routes.

`DirectedHandoff` adds the exact owner of each upstream dependency that was
produced by another employee, plus only the smallest specialist set needed to
close a capability gap. The dependency assignment is read from the same
generation-fenced project aggregate; a caller cannot substitute a preferred
handoff partner. Information moves only along explicit owner-to-specialist and
specialist-to-owner routes.

`ParallelIndependentReview` preserves separate evidence channels for material
risk, evidence conflict, or separation of duties. Equal agent IDs are not the
test of independence: model family, mandate, prompt, tool set, data provenance,
and prior-claim correlation all contribute to the correlation penalty.
High risk, non-low ambiguity or uncertainty, and evidence conflict require at
least two distinct evidence channels; if no such channel is eligible, the
decision escalates instead of silently falling back to `Solo`.

Uncertainty is derived from durable project state rather than caller prose. An
unresolved project-wide or work-local question, or an open blocker, is
`material`; an escalated blocker is `blocking`; resolved or unrelated records
do not raise the task classification. The workflow store recomputes the same
classification before commit so a stale or privileged internal caller cannot
weaken or arbitrarily strengthen the policy input.

`SpecialistPanel` is reserved for accepted work contracts whose accountable
owner cannot alone cover several complementary requirements, or whose result
depends on at least two other accountable producers. A multi-capability owner
does not become a panel merely because the capability list has multiple
entries. A panel requires at least three eligible participants and remains
sparse and owner-centered rather than creating an all-to-all room.

`HumanEscalation` is a terminal admission result when authority, privacy,
irreversible effect, or policy cannot be resolved safely by agents. An
irreversible effect always requires human authority. The mode does not invent a
team or consensus.

## Deterministic Selection

Hard constraints run before scoring. A candidate is ineligible when inactive,
outside the assignment authority, bound to a stale organization generation,
at the load limit, missing privacy clearance, missing a required capability,
without the required runtime or tools, or outside the cost budget.

The policy exhaustively evaluates the bounded candidate set. A valid set must:

1. contain the accountable owner;
2. cover every required capability;
3. satisfy every role and distinct-evidence separation rule;
4. remain within participant, cost, and time budgets.

The objective rewards capability coverage, evidence diversity, and calibrated
task reliability, then subtracts participant cost, queue delay, and correlation.
After finding the strongest score, the policy keeps only teams within the
declared quality tolerance and chooses the smallest one. Stable agent-ID order
breaks any remaining tie, making replay deterministic.

## Reliability Evidence

A reliability observation is task-specific evidence, not employee reputation.
It binds one agent, capability, task family, input class, immutable claim,
accepted output digest, independent verification digest, verifier authority,
evidence quality, and policy generation.

The store accepts an observation only when all of the following are true:

- the attributed claim exists in a collaboration session for the exact work
  item and was authored by the observed agent;
- the exact output digest belongs to a work item in `Done` state;
- the work item has a passing independent QA or Release gate;
- the verifier is a different authorized QA or Release participant;
- the capability belongs to the accepted work contract.

Rejected, self-approved, stale, or unverified outcomes cannot update routing.
Sparse evidence remains `Unknown`. Learned weighting stays disabled until #742
proves calibration and protected-dissent behavior and a maintainer activates
the policy generation. Reliability can influence routing but can never grant a
role, capability, privacy class, tool, or decision authority.

## Routes And Termination

Every non-solo decision creates only private directed routes and lists the
packet classes permitted on each edge. Runtime dispatch and result acceptance
must present the exact decision fence. Packets outside an edge, class, or
visibility contract fail closed.

Each decision carries participant, round, token, cost, deadline, novelty, and
stalled-progress limits. Public progress requests cannot submit round, token,
or cost counters. The server charges exactly one round for each accepted
progress transition; token and cost authority remains with the durable
provider-usage and project-reservation path. Internal accounting may advance
those counters only from that authority. Repeated evidence, duplicate work,
insufficient new information, exhausted budgets, explicit cancellation, or
escalation produces a typed terminal state. Terminal transitions release every
reservation exactly once. Every progress transition consumes at least one of
at most 64 admitted rounds, so rotating identifiers cannot create an unbounded
loop. The policy never converts agreement count or verbosity into consensus.

## Persistence And Publication

Admission, progression, request bindings, and capacity reservations are part of
the versioned project aggregate. SQLite `BEGIN IMMEDIATE` serializes competing
admissions so only one transaction can consume a given project version and the
latest cross-project agent capacity. A failed transaction leaves no decision,
reservation, event, or projection side effect.

Every admission and progress transition creates an immutable append proposal
for the canonical #731 Event Store stream in the same project transaction. The
proposal binds tenant, project, work item, admission ID, transition sequence,
command digest, organization authority, assignment authority, and expected
stream revision. If Event Store adoption is interrupted, the daemon replays the
same proposal; operation idempotency prevents a second event.

Public request retries are bound by operation ID and public request digest. A
valid retry returns the immutable response sealed for the original operation,
not a later project snapshot. Reusing the operation ID with different content
is an idempotency conflict.

An admitted collaboration session is bound to the exact admission decision,
selected participant capability snapshots, sparse routes, work item, and
admission generation. A newer admission for the same work item fences the old
session before further dispatch or result acceptance; an unrelated work item
does not interrupt a valid session. Session limits may only narrow the admitted
participant, claim, handoff, clarification, transition, and deadline bounds.

Pre-admission session records remain readable under their original version-1
binding digest so historical evidence is not rewritten. They are audit-only:
new collaboration mutations and Gateway requests require a complete version-2
admission binding, and terminal sessions cannot dispatch new work.

## API And Operations

Authenticated agents use:

```text
POST /company/workflow/collaboration/admissions
GET  /company/workflow/collaboration/admissions?project_id=<id>&admission_id=<id>
```

Only Project Management or Technical Lead authority can commit admission.
Selected agents may submit bounded evidence, milestone, blocker, and escalation
updates. They cannot self-report accounting counters or relax policy ceilings.
Only the exact work owner or Project Management/Technical Lead authority may
complete or cancel the admission as a whole; temporary team membership never
creates decision authority. Admission details are visible only to an operator,
Project Management, Technical Lead, or an admitted participant.
The generic company command endpoints reject internal admission, progression,
and reliability commands.

Deployment activates one versioned policy generation. Rollback disables
multi-agent admission and returns uncertain work to deterministic `Solo` or
required human/separation behavior. A policy-generation rollback does not
rewrite claims, outcomes, decisions, events, or observations already committed
to the product stores. The issue-acceptance VM snapshot is a separate deployment
safety boundary and makes no claim that post-snapshot observations survive a
machine-level restore.

The deployed product acceptance uses
`build_collaboration_admission_journey.py` to extend the accepted M0 journey
without replacing or interleaving any of its HTTP, authentication, replay,
Workbench, QA, delivery, or evidence contracts. The generated 63-step
token-free journey first completes the original customer delivery byte for
byte. It then creates a separate governed admission-validation project with a
capable solo owner, one real capability gap, an independent QA task, and a
two-specialist technical integration task. It admits and completes `Solo`,
`DirectedHandoff`, `ParallelIndependentReview`, and `SpecialistPanel`. Every
project version is read back from the product API rather than predicted by the
test. A projection boundary before the panel then proves the exact admitted and
completed Event Store updates, and a final projection readback proves all four
terminal policy states and their projection digest. The pinned designer and QA
agent configurations must contain every tool
capability required by their assigned Workbench profiles before the journey is
eligible to run.

The solo comparison is governed by the checked-in
`collaboration-admission-study-v1.json` contract. It fixes three task classes,
the candidate mode, participant ceiling, 95 percent confidence level, a
minimum of 20 paired trials, and the resource ceiling before any observation
exists. The zero-tolerance authority/privacy rule and both adoption rules are
part of that immutable contract. Each arm binds the exact deployed release,
policy generation, trial,
observed mode, accepted deliverable, risk-control result, authority/privacy
outcome, inference and resource counters, journey run and plan, durable ledger,
canonical event readback, projection readback, and result readback. The
evidence digest covers that complete observation rather than acting as an
uninterpreted label.

`evaluate_collaboration_admission.py` reports Wilson intervals for each arm and
uses a separate Wilson interval over discordant paired outcomes for adoption.
A larger mode is adopted only when the lower bound of its win rate among
discordant pairs exceeds one half for quality or the predeclared risk control,
every candidate trial selected the declared bounded mode, no authority or
privacy regression occurred, and every mean resource counter stayed at or
below the solo mean multiplied by the predeclared participant ceiling. A zero
solo mean consequently permits no added use of that resource. Equal outcomes,
too few informative pairs, excessive resource growth, or ambiguous evidence
therefore retain `Solo`. That result disables optional use of the larger mode;
it never permits `Solo` to bypass a capability, privacy, authority, or
separation constraint. Work that cannot satisfy a hard constraint blocks or
escalates. Runtime counters come from `.240`; builder timings are never
evidence. The evaluator does not turn policy assertions or agent self-reports
into outcome evidence.

Because each arm contains the same number of paired trials, the resource guard
compares exact integer sums before reporting rounded means. A sub-unit overrun
therefore cannot disappear through presentation rounding.
