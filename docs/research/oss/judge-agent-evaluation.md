# OSS Judge, Agent-Evaluation, and Adversarial-Testing Study

Status: research corrected and implementation contracts materialized for ORC review
Issue: [#717](https://github.com/silentspike/project-sentinel/issues/717)
Parent: [#659](https://github.com/silentspike/project-sentinel/issues/659)
Sentinel baseline: `55ace5371a64d4369dccf7aea13ceb32ae441891`
Research date: 2026-07-29

## Executive decision

Sentinel should not add any of the reviewed systems as a runtime dependency.
Their best mechanisms are useful, but none supplies Sentinel's authoritative
company-work, exact-candidate-digest, separation-of-duty, side-effect, replay,
and customer-acceptance contracts.

The recommended direction is:

1. **Port algorithm/contract** for the versioned evaluation-record and
   scorer-result contracts from Inspect AI and the explicit seed discipline
   from lm-evaluation-harness into a small Sentinel-owned QA schema.
2. **Reimplement minimal** calibrated model grading: deterministic assertions
   remain authoritative where possible; model grades retain grader identity,
   prompt revision, raw verdict, parse status, and disagreement.
3. **Integrate** evaluation with Sentinel's existing event, artifact, sandbox,
   and lineage records instead of copying trajectories into a second truth
   store.
4. **Port algorithm/contract** only for adversarial-generation and
   deterministic reduction patterns from promptfoo and garak. Do not copy
   their corpora without separate provenance, license, maintenance, and
   security review.
5. **Keep Sentinel** as the owner of product release and customer acceptance.
   External benchmark success must never substitute for the exact-digest QA
   contract in [#696](https://github.com/silentspike/project-sentinel/issues/696)
   and [#650](https://github.com/silentspike/project-sentinel/issues/650).

This is a source-backed design recommendation, not a benchmark result. No
upstream runtime was deployed, no Sentinel VM was accessed, and no performance
claim is derived from development or build-server hardware.

## Method and decision rules

### Evidence standard

- Sentinel claims are pinned to the current baseline and cite source or tests
  by file and line.
- Upstream claims are pinned to exact commits and cite implementation and tests,
  not documentation alone.
- Repository activity and licenses are screening inputs, not correctness proof.
- An upstream test proves only the tested upstream behavior. It does not prove
  Sentinel integration, security, or release readiness.
- Performance characteristics are hypotheses until measured on an authorized
  Sentinel runtime target under a separate implementation issue.
- A dependency recommendation requires a runtime owner, upgrade path, security
  boundary, and demonstrated advantage over a small owned contract.

### Screening rubric

Each criterion is scored from 0 (absent) to 3 (strong and source-backed):

| Criterion | Question |
|---|---|
| Data and scoring | Are datasets, fixtures, scorer results, and aggregation explicit? |
| Agent traces | Are tool calls, trajectories, side effects, and provenance represented? |
| Adversarial depth | Are attacks systematic, extensible, and regression-friendly? |
| Reproducibility | Are revisions, seeds, caches, retries, and partial failures explicit? |
| Isolation and operations | Can execution be bounded, isolated, audited, and kept offline? |
| Maintenance and license | Is the project active, permissively licensed, and operationally ownable? |

The score is a comparison aid, not an adoption threshold. A high score cannot
override a failed Sentinel authority or security fit.

## Sentinel baseline

### Current mechanisms and limits

| Surface | Source-backed behavior | Evaluation consequence |
|---|---|---|
| Realtime drift | [`CheckDrift`](https://github.com/silentspike/project-sentinel/blob/55ace5371a64d4369dccf7aea13ceb32ae441891/pkg/sentinel-go/judge/drift.go#L43-L121) returns clean for an unknown agent or empty history, then derives drift from exclamation count and message length with fixed cutoffs. | Useful operational heuristic, but fail-open unknown identities, fixed thresholds, and no calibration dataset prevent use as an independent acceptance oracle. |
| Message quality | [`ScoreMessage`](https://github.com/silentspike/project-sentinel/blob/55ace5371a64d4369dccf7aea13ceb32ae441891/pkg/sentinel-go/judge/quality.go#L34-L99) averages length, uppercase/digit "specificity", and the drift heuristic into a 1-5 score. | Deterministic and cheap, but it measures proxies rather than task correctness. |
| Fatigue | [`CheckFatigue`](https://github.com/silentspike/project-sentinel/blob/55ace5371a64d4369dccf7aea13ceb32ae441891/pkg/sentinel-go/judge/fatigue.go#L20-L97) uses repeated 20-character prefixes and message-length decline. | Suitable as a signal, not a release-quality metric. |
| Model swap | [`SwapTrigger`](https://github.com/silentspike/project-sentinel/blob/55ace5371a64d4369dccf7aea13ceb32ae441891/pkg/sentinel-go/judge/swap.go#L14-L103) keeps in-memory score history and uses fixed model names and consecutive-score thresholds. | It is a runtime reaction path, not a reproducible model comparison record. |
| Batch judge | [`BatchHandler.Analyze`](https://github.com/silentspike/project-sentinel/blob/55ace5371a64d4369dccf7aea13ceb32ae441891/services/sentinel-judge/internal/service/batch.go#L63-L142) runs heuristics and optional LLM analyses. Individual LLM failures are logged while the batch still returns success with missing fields. | Partial success is not represented as a typed evaluation outcome. A release gate could otherwise mistake absence for a passing result. |
| LLM analysis persistence | The analyzer parses one JSON response and logs, but does not propagate, evolution-write failure ([voice example](https://github.com/silentspike/project-sentinel/blob/55ace5371a64d4369dccf7aea13ceb32ae441891/services/sentinel-judge/internal/analyzer/analyzer.go#L69-L110)). | Grader output and durable side effect can diverge without a single atomic evaluation record. |
| Stream processing | The judge acknowledges malformed, empty, and processed messages; heuristic persistence failures are warnings ([consumer](https://github.com/silentspike/project-sentinel/blob/55ace5371a64d4369dccf7aea13ceb32ae441891/services/sentinel-judge/internal/service/stream.go#L123-L180), [writes](https://github.com/silentspike/project-sentinel/blob/55ace5371a64d4369dccf7aea13ceb32ae441891/services/sentinel-judge/internal/service/stream.go#L215-L316)). | Correct for best-effort monitoring, but incompatible with fail-closed QA evidence without a separate contract. |
| Fourth-wall judge | The gateway sends one temperature-zero grading call and parses one JSON object ([judge adapter](https://github.com/silentspike/project-sentinel/blob/55ace5371a64d4369dccf7aea13ceb32ae441891/cmd/cortex-gateway/internal/detection/judge.go#L9-L45)). | There is no grader revision, calibration set, disagreement record, or repeated-vote contract. |
| Gateway response gate | Judge errors on the synthesis path can permit forwarding, while the normal response path can regenerate boundedly ([pipeline](https://github.com/silentspike/project-sentinel/blob/55ace5371a64d4369dccf7aea13ceb32ae441891/cmd/cortex-gateway/internal/proxy/pipeline.go#L1166-L1195)). | Runtime availability policy and independent QA policy must remain separate. |
| MARBLE observatory | Observatory communication and personality metrics reuse the same quality and drift heuristics ([metrics](https://github.com/silentspike/project-sentinel/blob/55ace5371a64d4369dccf7aea13ceb32ae441891/cmd/cortex-gateway/internal/observatory/metrics.go#L50-L89)); its configuration fixes three model/provider shifts ([config](https://github.com/silentspike/project-sentinel/blob/55ace5371a64d4369dccf7aea13ceb32ae441891/cmd/cortex-gateway/internal/observatory/config.go#L11-L90)). | It is a comparative telemetry surface, not an independently calibrated benchmark or product release gate. |
| Nightrun | Integration tests cover consolidation, persistence, bounded queues, resume, event emission, and hash replay ([integration tests](https://github.com/silentspike/project-sentinel/blob/55ace5371a64d4369dccf7aea13ceb32ae441891/services/sentinel-nightrun/tests/integration.rs#L97-L419)). | Strong deterministic-state evidence, but deterministic simulation replay is not a task dataset, agent judge, or customer acceptance oracle. |

### Target-architecture requirements

The TOGAF guide requires MARBLE as the target multi-agent evaluation basis
([Cluster 09](https://github.com/silentspike/project-sentinel/blob/55ace5371a64d4369dccf7aea13ceb32ae441891/docs/architecture/togaf-architecture-guide.html#L2048-L2055))
and requires the target judge/nightrun path
([Cluster 08](https://github.com/silentspike/project-sentinel/blob/55ace5371a64d4369dccf7aea13ceb32ae441891/docs/architecture/togaf-architecture-guide.html#L1938-L1950)).
These are target requirements only, not evidence of current implementation.
The target does not yet specify the versioned evaluation, calibration, holdout,
authority, and retention contracts proposed below.

### Incident and limitation history

These closed items are evidence that evaluation must preserve provenance and
failure boundaries rather than merely report a score:

| Issue | Relevant lesson |
|---|---|
| [#278](https://github.com/silentspike/project-sentinel/issues/278) | LLM-backed nightrun work had to be removed from the deterministic tick path. Evaluation must not reintroduce blocking model calls into simulation authority. |
| [#296](https://github.com/silentspike/project-sentinel/issues/296) | Gateway MITM follow-ups required observability and redaction work. Eval prompts, outputs, traces, and credentials are security-sensitive records. |
| [#382](https://github.com/silentspike/project-sentinel/issues/382) | NMDA selection required threshold tuning and quality evidence. Fixed thresholds without a pinned corpus are not self-validating. |
| [#529](https://github.com/silentspike/project-sentinel/issues/529) | Replay across nightrun/shift boundaries required a specific deterministic-state correction. A successful eval must bind to the exact state and artifact lineage it evaluated. |
| [#27](https://github.com/silentspike/project-sentinel/issues/27) | The observatory issue is closed and verified but still carries `quality:needs-spec`. It cannot serve as the current general evaluation contract. |

### Existing owners and non-overlap

| Owner | Current contract | #717 boundary |
|---|---|---|
| [#26](https://github.com/silentspike/project-sentinel/issues/26) | Closed and verified Judge streaming, batch, persistence, alerting, metrics, and service-lifecycle work. | Establishes delivered operational history; its completion status does not prove the current evaluation design optimal or provide the proposed QA record. |
| [#27](https://github.com/silentspike/project-sentinel/issues/27) | Closed and verified MARBLE-style multi-agent observatory and comparison work, with `quality:needs-spec` retained. | Remains the owner of comparative observatory history, not product QA or exact-candidate release authority. |
| [#144](https://github.com/silentspike/project-sentinel/issues/144) | Closed and verified Cortex narrative nudge, personality guard, quality-gate, and bounded regeneration work. | Owns delivered response-path hardening; this study must not replace it or treat it as an independently calibrated release oracle. |
| [#296](https://github.com/silentspike/project-sentinel/issues/296) | Closed gateway MITM observability, redaction, and bounded operator-visibility work. | Its data-handling boundary constrains evaluation evidence; this study neither reopens the gateway implementation nor authorizes trace export. |
| [#650](https://github.com/silentspike/project-sentinel/issues/650) | M0 single-node product acceptance and exact-candidate QA. | Owns the product acceptance outcome; this study only proposes evaluation mechanisms. |
| [#696](https://github.com/silentspike/project-sentinel/issues/696) | Independent QA, release, customer delivery, and product lineage. | Primary implementation owner for evaluation records, scorer evidence, and release policy. |
| [#693](https://github.com/silentspike/project-sentinel/issues/693) | Verified M0 work-execution contract and conformance matrix. | Defines the requirement-to-owner/evidence contract; it is not a runtime data store. |
| [#694](https://github.com/silentspike/project-sentinel/issues/694) | Workbench execution, tool effects, sandboxing, invocation recovery, and artifact references. | Owns the authoritative execution evidence that QA must read, not copy. |
| [#695](https://github.com/silentspike/project-sentinel/issues/695) | Customer/project workflow, decisions, handoffs, approvals, and completion evidence. | Owns authoritative workflow evidence and the delivery-candidate transition. |
| [#709](https://github.com/silentspike/project-sentinel/issues/709) / [#731](https://github.com/silentspike/project-sentinel/issues/731) | Event envelopes, expected-revision append, outbox/inbox outcomes, projection generations, poison handling, and event-truth retention. | QA lifecycle records use this durability substrate; #717 does not create a second event or trajectory store. |
| [#710](https://github.com/silentspike/project-sentinel/issues/710) | Durable execution, external-effect receipts, idempotent resume, and cross-store recovery semantics. | Owns crash/retry/effect execution semantics used by active QA scenarios. |
| [#722](https://github.com/silentspike/project-sentinel/issues/722) | Whole-product backup, checkpoint, Time Machine, and disaster-recovery target contract. | Must include QA plans, run receipts, fixtures, evidence references, and release manifests in a recoverable generation. |
| [#706](https://github.com/silentspike/project-sentinel/issues/706) | Supervision, readiness, repair verification, quarantine, and escalation target contract. | Owns fail-closed readiness/quarantine when required QA authority or durable dependencies are unavailable. |
| [#736](https://github.com/silentspike/project-sentinel/issues/736) | Mandatory consumer frontiers, retention proofs, backup cuts, and generation-bound recovery. | Required QA/evidence frontiers must block pruning until release, rollback, audit, and customer-retention policy permit retirement. |
| [#393](https://github.com/silentspike/project-sentinel/issues/393) | Formal verification of deterministic critical cores. | Complements, but explicitly cannot replace, probabilistic model evaluation. |
| [#705](https://github.com/silentspike/project-sentinel/issues/705) | Dependency necessity and ownership audit. | Any later dependency proposal must route through this owner. |
| [#656](https://github.com/silentspike/project-sentinel/issues/656) | Dependency upgrade operations. | Owns upgrades only after an approved dependency exists. |

## OSS landscape

### Reproducible candidate inventory

Repository state and GitHub's latest-release endpoint were read on 2026-07-29.
"Deep" means that pinned source and tests were reviewed below. Release absence
means the endpoint returned no published latest release; it does not imply
inactivity.

| Candidate | Pin | License | Pin date | Latest release at review | Score / 18 | Deep | Disposition |
|---|---|---|---:|---|---:|---:|---|
| [Inspect AI](https://github.com/UKGovernmentBEIS/inspect_ai/tree/8f5a9f2e4d18a657ed5df4118c3d126f3eead440) | `8f5a9f2e4d18a657ed5df4118c3d126f3eead440` | MIT | 2026-07-28 | None published | 16 | Yes | Best contract source for eval records, limits, retries, scorers, and sandboxed agent traces; port contracts, do not add runtime dependency. |
| [promptfoo](https://github.com/promptfoo/promptfoo/tree/ac8971fcfa961fa5fa96bcc4f527f5309b504997) | `ac8971fcfa961fa5fa96bcc4f527f5309b504997` | MIT | 2026-07-28 | [`0.121.19`](https://github.com/promptfoo/promptfoo/releases/tag/0.121.19) | 15 | Yes | Best adversarial and trajectory-assertion reference; port selected patterns only. |
| [lm-evaluation-harness](https://github.com/EleutherAI/lm-evaluation-harness/tree/f4d4b3de3ee6741a7151a9fe74945ee515262f4c) | `f4d4b3de3ee6741a7151a9fe74945ee515262f4c` | MIT | 2026-07-13 | [`v0.4.12`](https://github.com/EleutherAI/lm-evaluation-harness/releases/tag/v0.4.12) | 11 | Yes | Strong classic task/seed/cache discipline; poor fit for tool side effects and company workflow. |
| [DeepEval](https://github.com/confident-ai/deepeval/tree/0d100e37d4263f208488f3c13e15561bce3b694f) | `0d100e37d4263f208488f3c13e15561bce3b694f` | Apache-2.0 | 2026-07-28 | [`v4.1.3`](https://github.com/confident-ai/deepeval/releases/tag/v4.1.3) | 13 | Yes | Useful agent and G-Eval reference; model-heavy scoring and Python/cloud surface make direct adoption unattractive. |
| [garak](https://github.com/NVIDIA/garak/tree/0b51f87acda1c0ab22a88dff6fd304f3299c9ce4) | `0b51f87acda1c0ab22a88dff6fd304f3299c9ce4` | Apache-2.0 | 2026-07-28 | [`v0.15.1`](https://github.com/NVIDIA/garak/releases/tag/v0.15.1) | 12 | Yes | Strong attack-probe/detector and calibration reference; scanner workflow is not a product QA workflow. |
| [Microsoft PyRIT](https://github.com/microsoft/PyRIT/tree/0d239528377dc3216f27d074a730551ab037185c) | `0d239528377dc3216f27d074a730551ab037185c` | MIT | 2026-07-28 | [`v1.0.0`](https://github.com/microsoft/PyRIT/releases/tag/v1.0.0) | 13 | No | Serious red-team orchestrator, but overlaps promptfoo/garak and adds a broad Python orchestration/data layer. Retain as a future security-owner reference, not this study's primary mechanism source. |
| [OpenAI Evals](https://github.com/openai/evals/tree/8eac7a7de5215c907fbddc30efdaf316913eccdd) | `8eac7a7de5215c907fbddc30efdaf316913eccdd` | MIT for code; dataset-specific exceptions | 2026-04-14 | None published | 8 | No | Historically influential model-graded patterns, but weaker current agent/operations fit, a legacy event model, and dataset-specific license review. Reject dependency. |
| [AgentBench](https://github.com/THUDM/AgentBench/tree/d1e4a10db08c87075c78972e48ecc182be03e2d5) | `d1e4a10db08c87075c78972e48ecc182be03e2d5` | Apache-2.0 | 2026-02-08 | None published | 9 | No | Valuable benchmark environments, but heavyweight environment setup and benchmark-specific success metrics do not map to Sentinel release authority. Reject integration. |

Score detail:

| Candidate | Data/scoring | Agent traces | Adversarial | Reproducibility | Isolation/ops | Maintenance/license |
|---|---:|---:|---:|---:|---:|---:|
| Inspect AI | 3 | 3 | 1 | 3 | 3 | 3 |
| promptfoo | 3 | 3 | 3 | 2 | 1 | 3 |
| lm-evaluation-harness | 3 | 0 | 0 | 3 | 2 | 3 |
| DeepEval | 2 | 3 | 2 | 2 | 1 | 3 |
| garak | 2 | 1 | 3 | 3 | 1 | 2 |
| Microsoft PyRIT | 2 | 2 | 3 | 2 | 1 | 3 |
| OpenAI Evals | 2 | 0 | 1 | 2 | 1 | 2 |
| AgentBench | 2 | 3 | 1 | 2 | 0 | 1 |

### Source-backed rejection checks

These candidates were screened below the deep-review cutoff, but their
dispositions still come from pinned implementation and tests rather than
project descriptions alone.

**Microsoft PyRIT.** The scenario layer persists a versioned identity, attack
results, labels, dataset choices, retry state, and resume identity through a
process-global `CentralMemory`
([scenario initialization](https://github.com/microsoft/PyRIT/blob/0d239528377dc3216f27d074a730551ab037185c/pyrit/scenario/core/scenario.py#L519-L670),
[memory singleton](https://github.com/microsoft/PyRIT/blob/0d239528377dc3216f27d074a730551ab037185c/pyrit/memory/central_memory.py#L11-L44)).
Its scorer evaluator versions human-labeled datasets, repeats model trials,
computes agreement metrics, and can reuse prior scorer results
([evaluator](https://github.com/microsoft/PyRIT/blob/0d239528377dc3216f27d074a730551ab037185c/pyrit/score/scorer_evaluation/scorer_evaluator.py#L109-L230)).
Tests exercise retry, partial completion, persisted resume, and scorer
agreement
([retry](https://github.com/microsoft/PyRIT/blob/0d239528377dc3216f27d074a730551ab037185c/tests/unit/scenario/core/test_scenario_retry.py#L227-L299),
[partial completion](https://github.com/microsoft/PyRIT/blob/0d239528377dc3216f27d074a730551ab037185c/tests/unit/scenario/core/test_scenario_partial_results.py#L130-L200),
[scorer metrics](https://github.com/microsoft/PyRIT/blob/0d239528377dc3216f27d074a730551ab037185c/tests/unit/score/test_scorer_evaluator.py#L68-L108)).
This is meaningful evidence, but direct integration would duplicate Sentinel's
workbench, event, artifact, and QA ownership with a large Python/Azure/provider
surface. The repository is MIT licensed and has a coordinated disclosure
policy
([license](https://github.com/microsoft/PyRIT/blob/0d239528377dc3216f27d074a730551ab037185c/LICENSE),
[security](https://github.com/microsoft/PyRIT/blob/0d239528377dc3216f27d074a730551ab037185c/SECURITY.md)).
Keep it as a security-owner reference; do not add it as a dependency here.

**OpenAI Evals.** Its core evaluation loop uses fixed sample shuffling,
per-sample seeded RNG, bounded concurrency, and per-sample solver copies
([evaluation loop](https://github.com/openai/evals/blob/8eac7a7de5215c907fbddc30efdaf316913eccdd/evals/eval.py#L26-L147),
[solver isolation](https://github.com/openai/evals/blob/8eac7a7de5215c907fbddc30efdaf316913eccdd/evals/eval.py#L168-L253)).
The model-graded classifier records a choice and score but averages only
non-null scores
([classifier](https://github.com/openai/evals/blob/8eac7a7de5215c907fbddc30efdaf316913eccdd/evals/elsuite/modelgraded/classify.py#L53-L125)).
The recorder represents errors, can suppress selected sensitive fields, and
falls back from HTTP delivery to a local file
([recording](https://github.com/openai/evals/blob/8eac7a7de5215c907fbddc30efdaf316913eccdd/evals/record.py#L245-L257),
[local and fallback paths](https://github.com/openai/evals/blob/8eac7a7de5215c907fbddc30efdaf316913eccdd/evals/record.py#L316-L465)).
Representative core tests check hidden-field serialization and aggregate
accuracy
([recorder test](https://github.com/openai/evals/blob/8eac7a7de5215c907fbddc30efdaf316913eccdd/evals/record_test.py#L8-L29),
[metric test](https://github.com/openai/evals/blob/8eac7a7de5215c907fbddc30efdaf316913eccdd/tests/unit/evals/test_metrics.py#L10-L24)).
Those tests do not establish Sentinel tool-side-effect authority, calibrated
grader disagreement, or company release lineage.
The code is MIT licensed, while bundled datasets retain separate licenses
([license boundary](https://github.com/openai/evals/blob/8eac7a7de5215c907fbddc30efdaf316913eccdd/LICENSE.md#L1-L40));
a security policy exists
([security](https://github.com/openai/evals/blob/8eac7a7de5215c907fbddc30efdaf316913eccdd/SECURITY.md)).
The useful seed and event concepts are already covered more completely by the
deep shortlist, so reject another framework and data-corpus dependency.

**AgentBench.** The scheduler resumes from JSONL, assigns task samples across
agent and worker capacity, retries failures, and writes per-task summaries
([assigner](https://github.com/THUDM/AgentBench/blob/d1e4a10db08c87075c78972e48ecc182be03e2d5/src/assigner.py#L41-L159),
[worker allocation](https://github.com/THUDM/AgentBench/blob/d1e4a10db08c87075c78972e48ecc182be03e2d5/src/assigner.py#L161-L299),
[retry and output](https://github.com/THUDM/AgentBench/blob/d1e4a10db08c87075c78972e48ecc182be03e2d5/src/assigner.py#L301-L383)).
Its task client drives a remote environment turn by turn and preserves the
latest output on typed network, agent, start, and interaction failures
([task client](https://github.com/THUDM/AgentBench/blob/d1e4a10db08c87075c78972e48ecc182be03e2d5/src/client/task.py#L10-L125)).
The pinned CI smoke check validates only the lightweight YAML topology, not
task execution or scoring
([validator](https://github.com/THUDM/AgentBench/blob/d1e4a10db08c87075c78972e48ecc182be03e2d5/scripts/validate_lite_configs.py#L1-L107),
[workflow](https://github.com/THUDM/AgentBench/blob/d1e4a10db08c87075c78972e48ecc182be03e2d5/.github/workflows/lite-configs.yml#L1-L26)).
The Apache-2.0 repository has no root security policy at the pin. Its
containerized database, operating-system, knowledge-graph, and shopping
environments are benchmark products in their own right; they are not a thin
fit for Sentinel's exact-candidate company acceptance path. Reject integration,
while leaving task-environment ideas as non-authoritative hypotheses.

## Pinned deep reviews

### 1. Inspect AI

**Mechanisms.** `EvalConfig` makes sample selection, epochs, approvals, failure
thresholds, retries, token/turn/time/cost limits, concurrency, and sandbox
cleanup explicit
([source](https://github.com/UKGovernmentBEIS/inspect_ai/blob/8f5a9f2e4d18a657ed5df4118c3d126f3eead440/src/inspect_ai/log/_log.py#L91-L218)).
`EvalSample` records inputs, targets, sandbox, files, messages, output, scores,
events, usage, retries, errors, and the limit that stopped execution
([source](https://github.com/UKGovernmentBEIS/inspect_ai/blob/8f5a9f2e4d18a657ed5df4118c3d126f3eead440/src/inspect_ai/log/_log.py#L395-L528)).
The versioned `EvalLog` ties configuration, plan, results, usage, errors, and
sample records together
([source](https://github.com/UKGovernmentBEIS/inspect_ai/blob/8f5a9f2e4d18a657ed5df4118c3d126f3eead440/src/inspect_ai/log/_log.py#L1123-L1170)).

**Scoring and failure behavior.** Model graders can vote across multiple models;
a parse failure becomes explicitly unscored rather than a pass
([source](https://github.com/UKGovernmentBEIS/inspect_ai/blob/8f5a9f2e4d18a657ed5df4118c3d126f3eead440/src/inspect_ai/scorer/_model.py#L87-L151),
[parse path](https://github.com/UKGovernmentBEIS/inspect_ai/blob/8f5a9f2e4d18a657ed5df4118c3d126f3eead440/src/inspect_ai/scorer/_model.py#L173-L240)).
Tests cover parse failure, grade extraction, model-role precedence, delimiter
injection, and checkpoint resume during scoring
([grader tests](https://github.com/UKGovernmentBEIS/inspect_ai/blob/8f5a9f2e4d18a657ed5df4118c3d126f3eead440/tests/scorer/test_model_graded.py#L127-L160),
[injection tests](https://github.com/UKGovernmentBEIS/inspect_ai/blob/8f5a9f2e4d18a657ed5df4118c3d126f3eead440/tests/scorer/test_model_graded.py#L440-L526),
[resume test](https://github.com/UKGovernmentBEIS/inspect_ai/blob/8f5a9f2e4d18a657ed5df4118c3d126f3eead440/tests/checkpoint/test_checkpoint_scoring_resume_e2e.py#L108-L153)).

**Security and operations.** Sandboxes, approval policies, egress controls, and
resource limits are first-class, but they remain operator-configured. The
framework can execute model-generated tool work and therefore must not run in a
Sentinel authority process. The repository contains no root `SECURITY.md` at
the pin. License is MIT
([license](https://github.com/UKGovernmentBEIS/inspect_ai/blob/8f5a9f2e4d18a657ed5df4118c3d126f3eead440/LICENSE)).

**Decision impact.** Port the record shape, typed non-score outcomes, limits,
and retry provenance. Do not import the Python runtime or create a second
artifact/event authority.

### 2. promptfoo

**Mechanisms.** The trajectory assertions validate named tools, ordered tool
sequences, argument matching, step counts, and model-graded goal success
([source](https://github.com/promptfoo/promptfoo/blob/ac8971fcfa961fa5fa96bcc4f527f5309b504997/src/assertions/trajectory.ts#L41-L72),
[tool sequence](https://github.com/promptfoo/promptfoo/blob/ac8971fcfa961fa5fa96bcc4f527f5309b504997/src/assertions/trajectory.ts#L431-L604),
[goal success](https://github.com/promptfoo/promptfoo/blob/ac8971fcfa961fa5fa96bcc4f527f5309b504997/src/assertions/trajectory.ts#L635-L649)).
Its evaluator provides repeat indices, per-step identity, rate-limit scheduling,
timeouts, traces, assertion aggregation, token accounting, and persisted
results
([source](https://github.com/promptfoo/promptfoo/blob/ac8971fcfa961fa5fa96bcc4f527f5309b504997/src/evaluator.ts#L720-L752),
[provider call](https://github.com/promptfoo/promptfoo/blob/ac8971fcfa961fa5fa96bcc4f527f5309b504997/src/evaluator.ts#L851-L958)).

**Tests and failure behavior.** Tests verify that goal, trajectory, and output
cannot be overridden by caller variables and that thresholds are applied
([tests](https://github.com/promptfoo/promptfoo/blob/ac8971fcfa961fa5fa96bcc4f527f5309b504997/test/matchers/trajectory-goal-success.test.ts#L12-L125)).
The evaluator can represent provider errors and assertion failures, but its
large provider/plugin surface makes Sentinel-specific fail-closed review
expensive.

**Security and operations.** Some attack generation and grading can use remote
services. The code provides explicit global and red-team-specific disable
switches, but also prefers remote generation for cloud users or absent local
credentials
([source](https://github.com/promptfoo/promptfoo/blob/ac8971fcfa961fa5fa96bcc4f527f5309b504997/src/redteam/remoteGeneration.ts#L63-L84),
[selection](https://github.com/promptfoo/promptfoo/blob/ac8971fcfa961fa5fa96bcc4f527f5309b504997/src/redteam/remoteGeneration.ts#L155-L182)).
Any trial would require telemetry, sharing, remote generation, and cloud paths
disabled and verified. License is MIT and a security policy exists
([license](https://github.com/promptfoo/promptfoo/blob/ac8971fcfa961fa5fa96bcc4f527f5309b504997/LICENSE),
[security](https://github.com/promptfoo/promptfoo/blob/ac8971fcfa961fa5fa96bcc4f527f5309b504997/SECURITY.md)).

**Decision impact.** Port assertion semantics and attack-reduction patterns.
Do not adopt the Node runtime, database, cloud paths, or plugin supply chain.

### 3. lm-evaluation-harness

**Mechanisms.** A task owns dataset loading, splits, prompting, targets,
few-shot selection, requests, and metrics
([source](https://github.com/EleutherAI/lm-evaluation-harness/blob/f4d4b3de3ee6741a7151a9fe74945ee515262f4c/lm_eval/api/task.py#L64-L161)).
Request-cache keys include task, few-shot count, rank/world size, chat template,
system prompt hash, and tokenizer
([source](https://github.com/EleutherAI/lm-evaluation-harness/blob/f4d4b3de3ee6741a7151a9fe74945ee515262f4c/lm_eval/api/task.py#L268-L310)).
The evaluator exposes separate Python, NumPy, Torch, and few-shot seeds and
records them in output configuration
([source](https://github.com/EleutherAI/lm-evaluation-harness/blob/f4d4b3de3ee6741a7151a9fe74945ee515262f4c/lm_eval/evaluator.py#L55-L87),
[seed application](https://github.com/EleutherAI/lm-evaluation-harness/blob/f4d4b3de3ee6741a7151a9fe74945ee515262f4c/lm_eval/evaluator.py#L197-L211)).

**Tests and failure behavior.** The test suite covers task construction,
request construction, metrics, aggregation, filtering, request caching, and
evaluator behavior
([task tests](https://github.com/EleutherAI/lm-evaluation-harness/blob/f4d4b3de3ee6741a7151a9fe74945ee515262f4c/tests/test_tasks.py#L59-L164),
[metric tests](https://github.com/EleutherAI/lm-evaluation-harness/blob/f4d4b3de3ee6741a7151a9fe74945ee515262f4c/tests/test_metrics.py#L56-L181),
[evaluator tests](https://github.com/EleutherAI/lm-evaluation-harness/blob/f4d4b3de3ee6741a7151a9fe74945ee515262f4c/tests/test_evaluator.py#L37-L130)).
This is strong evidence for language-model benchmark repeatability, but
requests are primarily model inputs/outputs; authoritative external side
effects and multi-agent workflow state are not the core abstraction.

**Security and operations.** Dataset and model integrations can download remote
code or artifacts. The evaluator has an explicit `confirm_run_unsafe_code`
input
([source](https://github.com/EleutherAI/lm-evaluation-harness/blob/f4d4b3de3ee6741a7151a9fe74945ee515262f4c/lm_eval/evaluator.py#L81-L86)),
but a Sentinel integration would still need offline pinning and sandboxing.
There is no root `SECURITY.md` at the pin. License is MIT
([license](https://github.com/EleutherAI/lm-evaluation-harness/blob/f4d4b3de3ee6741a7151a9fe74945ee515262f4c/LICENSE.md)).

**Decision impact.** Port seed and cache-key completeness checks. Reject direct
integration because it does not own agent side effects, company artifacts, or
customer acceptance.

### 4. DeepEval

**Mechanisms.** `GEval` accepts criteria or explicit evaluation steps, a
threshold, model, strict mode, and test-case fields
([source](https://github.com/confident-ai/deepeval/blob/0d100e37d4263f208488f3c13e15561bce3b694f/deepeval/metrics/g_eval/g_eval.py#L45-L89)).
Agent-focused test cases represent called and expected tools
([source](https://github.com/confident-ai/deepeval/blob/0d100e37d4263f208488f3c13e15561bce3b694f/deepeval/test_case/llm_test_case.py#L349-L405)).
Task-completion scoring derives goal/outcome and applies strict or thresholded
verdicts
([source](https://github.com/confident-ai/deepeval/blob/0d100e37d4263f208488f3c13e15561bce3b694f/deepeval/metrics/task_completion/task_completion.py#L25-L72)).

**Tests and failure behavior.** Tests cover G-Eval sync/async and multimodal
paths
([tests](https://github.com/confident-ai/deepeval/blob/0d100e37d4263f208488f3c13e15561bce3b694f/tests/test_metrics/test_g_eval_metric.py#L22-L210)),
task completion with called-tool evidence
([tests](https://github.com/confident-ai/deepeval/blob/0d100e37d4263f208488f3c13e15561bce3b694f/tests/test_metrics/test_task_completion_tools_called.py#L40-L147)),
and deterministic tool-permission outcomes including strict mode
([tests](https://github.com/confident-ai/deepeval/blob/0d100e37d4263f208488f3c13e15561bce3b694f/tests/test_metrics/test_tool_permission_metric.py#L15-L74)).
Most semantic metrics remain model-graded, so their apparent numeric precision
does not by itself establish calibration, independence, or deterministic
release behavior. Strict mode raises the threshold to 1, but does not make the
grader deterministic.

**Security and operations.** Agent traces and model prompts may contain company
or customer data. The broad integration and hosted-platform surface would need
an offline data-flow review before any trial. The repository has no root
`SECURITY.md` at the pin. License is Apache-2.0
([license](https://github.com/confident-ai/deepeval/blob/0d100e37d4263f208488f3c13e15561bce3b694f/LICENSE.md)).

**Decision impact.** Reimplement the useful grader-evidence fields and tool
expectation shape in Sentinel's schema. Do not adopt model-derived task
completion as the authoritative release decision.

### 5. garak

**Mechanisms.** `Attempt` stores conversation, prompt/output views, detector
results, notes, and serialization
([source](https://github.com/NVIDIA/garak/blob/0b51f87acda1c0ab22a88dff6fd304f3299c9ce4/garak/attempt.py#L173-L397)).
The harness composes generators, probes, detectors, evaluators, and JSONL
reporting
([source](https://github.com/NVIDIA/garak/blob/0b51f87acda1c0ab22a88dff6fd304f3299c9ce4/garak/harnesses/base.py#L82-L279)).
Calibration converts a probe/detector score into a z-score against pinned
calibration data
([source](https://github.com/NVIDIA/garak/blob/0b51f87acda1c0ab22a88dff6fd304f3299c9ce4/garak/analyze/calibration.py#L17-L119)).

**Tests and failure behavior.** Tests exercise conversation expansion,
serialization, report structure, plugin-cache provenance, and digest generation
([attempt tests](https://github.com/NVIDIA/garak/blob/0b51f87acda1c0ab22a88dff6fd304f3299c9ce4/tests/test_attempt.py#L150-L220),
[report provenance](https://github.com/NVIDIA/garak/blob/0b51f87acda1c0ab22a88dff6fd304f3299c9ce4/tests/test_internal_structures.py#L138-L199)).
Calibration load failure is represented by an unavailable calibration result,
which a Sentinel gate would need to reject explicitly.

**Security and operations.** The payload and plugin system intentionally handles
hostile prompts and many remote model backends. It belongs in an isolated test
lane, never an authority or production process. License is Apache-2.0 and a
security policy exists
([license](https://github.com/NVIDIA/garak/blob/0b51f87acda1c0ab22a88dff6fd304f3299c9ce4/LICENSE),
[security](https://github.com/NVIDIA/garak/blob/0b51f87acda1c0ab22a88dff6fd304f3299c9ce4/SECURITY.md)).

**Decision impact.** Port the probe/detector/result separation and calibrated
baseline metadata. Do not copy the payload corpus or plugin runtime.

## Mechanism matrix

`1:n` means one authoritative Sentinel source record may be referenced by many
evaluation results without copying it into another source of truth.

### Complete shortlist comparison

| System | Dataset, fixtures, scoring | Deterministic/model grading | Trajectories and side effects | Adversarial regression | Company E2E and release |
|---|---|---|---|---|---|
| Sentinel today | Has runtime heuristics, observatory records, nightrun tests, and product contracts, but no general versioned eval record. | Deterministic proxy heuristics plus one-shot LLM judges; no calibration or disagreement record. | Authoritative workflow/workbench/event/artifact sources are owned by #694/#695, but judge evidence is not yet bound to them. | Fourth-wall, drift, and security tests cover selected classes without a versioned attack registry or minimizer. | #650/#696 define exact-digest, independent-QA, release, delivery, and customer authority; this is the only domain-complete contract. |
| Inspect AI | Versioned eval config/log/sample/result records, epochs, typed failures, usage, and reducers. | Deterministic scorers plus single/multi-model grading; parse failure is unscored. | Messages, events, store, sandbox, files, limits, errors, and retries are retained per sample. | General scanner/adversarial corpus is not its primary strength. | Generic eval pass/fail has no Sentinel agreement, release, delivery, or customer state. |
| promptfoo | Test-suite variables, repeats, providers, assertions, result storage, traces, and token accounting. | Deterministic assertions and many model-graded rubrics; threshold behavior is explicit but grader calibration is operator-owned. | Strong tool-used, sequence, argument, count, and goal-success assertions over traces. | Broad plugins, strategies, transformations, and remote/local generation paths. | Generic evaluation thresholds lack exact Sentinel artifact/workflow/customer authority. |
| lm-evaluation-harness | Strong task/dataset/split/few-shot/metric/aggregation schema with explicit seeds and request caches. | Primarily deterministic benchmark metrics around model requests; custom extensions can add more, but judge disagreement is not a central contract. | Model request/response instances, not authoritative tool effects or company workflow. | No broad agent-policy adversarial registry. | Benchmark result schema has no product release or customer acceptance semantics. |
| DeepEval | Test cases and datasets cover text, conversations, tools, and traces; results can be local or integrated with hosted services. | Rich G-Eval and agent metrics plus deterministic policy metrics; most semantic scores remain model-derived. | Called/expected tools, roles, steps, and task outcomes are first-class test inputs. | Safety and role metrics exist, but attack generation/minimization is less complete than promptfoo/garak. | Generic test runs do not enforce Sentinel separation of duties, exact release digest, or customer action. |
| garak | Probes are cases; detector scores and JSONL/AVID reports retain plugin and model metadata. | Detectors include deterministic and model-based forms; calibration maps scores to pinned distributions. | Attempts retain conversations and outputs, not authoritative sandbox side effects. | Strongest focused probe/detector payload-scanning model in the shortlist. | Vulnerability reports do not implement company QA, promotion, delivery, rollback, or acceptance. |

### Shortlist cross-cutting fit

| System | Benefit and cost | Failure semantics | Determinism and 1:n fit | Security and operations | Maintenance/dependency impact | Expected Sentinel boundary |
|---|---|---|---|---|---|---|
| Inspect AI | Highest-value record/limit/retry design; high cost if the full Python runtime is adopted. | Typed sample/eval errors, retries, limits, unscored parse failures, and resumable logs. | Deterministic scorers are reproducible; model scorers are evidence. Its copied log would compete with Sentinel sources unless reduced to immutable refs. | Agent tools require configured sandboxes, approvals, egress, limits, and sensitive-log handling. No root security policy at the pin. | Active MIT project with a large Python dependency and execution surface. | Port schema/invariants only; isolated future reference harness at most. |
| promptfoo | Broad assertions and attack generation; large Node/provider/plugin/cloud surface. | Provider, assertion, threshold, and config failures are represented, but integration policy decides fail-open/closed behavior. | Deterministic assertions/repeats can be stable; model graders/generation are not. Traces should be referenced, not become authority. | Disable telemetry, sharing, remote generation, cloud and unsafe script/plugin paths for any isolated trial. | Active MIT project; frequent releases imply substantial upgrade and supply-chain ownership. | Port trajectory assertions and attack/minimization patterns only. |
| lm-evaluation-harness | Mature benchmark task discipline; low direct agent/company fit. | Integrity, task, cache, request, metric, and model failures are benchmark-run failures. | Explicit seed families and cache inputs are strong; no natural 1:n relation to Sentinel events/artifacts. | Remote datasets/models and optional unsafe/trusted code require offline pins and sandboxing. No root security policy at the pin. | Active MIT Python ecosystem with model-specific optional dependencies. | Port seed/cache completeness checks; no integration. |
| DeepEval | Rich semantic/agent metrics; model/API cost and hosted integration enlarge the boundary. | Metric errors and strict thresholds exist, but semantic verdicts remain grader-dependent. | Deterministic tool-policy metrics can replay; semantic metrics cannot be treated as deterministic truth. Trace uploads would duplicate or disclose source data. | Cloud API, telemetry, uploads, model prompts, and traces require explicit offline/opt-out verification. No root security policy at the pin. | Active Apache-2.0 project with broad Python/integration churn. | Reimplement minimal evidence fields and selected deterministic policies. |
| garak | Strong probe/detector/calibration separation; scanner operations do not map directly to product QA. | Detector/plugin/report/calibration failures remain scanner outcomes; unavailable calibration must fail closed in Sentinel. | Seeds and calibration metadata aid replay; attempt/report copies are not a 1:n authoritative source model. | Hostile corpora and remote generators require an isolated security lane and strict retention/secrets controls. | Active Apache-2.0 project with plugin/payload maintenance and a root security policy. | Port probe/result/calibration contracts and author Sentinel fixtures. |

### Mechanism-level requirements

| Mechanism | Current Sentinel | Strongest upstream evidence | Correctness and failure model | Determinism / 1:n | Security and operations | Integration and maintenance | Performance hypothesis only |
|---|---|---|---|---|---|---|---|
| 1. Dataset, fixtures, scoring, reproducibility | Heuristic scores and observatory records; no versioned general eval record. | Inspect versioned logs and typed sample errors; lm-eval seeds/cache/task schema. | A required case must end as `pass`, `fail`, `error`, `unscored`, `skipped`, `needs_human_review`, or `flaky_unresolved`; only policy-authorized `pass` can contribute to release. Dataset split, oracle, provenance, license, access, supersession, and retirement are versioned. | `EvaluationPlanDigest` canonically binds candidate, agreement/AC, required inventory, fixtures, evaluator, runner/toolchain, sandbox/environment, policy, and seeds. Deterministic result digests are separate from operational receipts and model evidence. Source refs remain 1:n and include content digests. | Development, calibration, hidden holdout, and adversarial-canary sets have separate access. Expected answers, rubrics, and blocking corpus are denied to candidate producers; leakage triggers quarantine, investigation, rotation, and supersession. | A Sentinel-owned schema uses #709/#731 event truth, #736 retention, and #722 recovery instead of a Python/Node authority. | Record volume and scoring cost require later authorized measurement; no estimate is accepted here. |
| 2. Deterministic and model-graded scoring, calibration, disagreement | Fixed heuristics plus one-shot LLM judges; no calibration/disagreement record. | Inspect explicit unscored parse failures and multi-model vote; DeepEval rubric shape; garak calibration metadata. | Deterministic assertions remain authoritative. Model grades are attributable evidence; unknown provider/model identity stays explicit. Unresolved disagreement or insufficient calibration becomes `needs_human_review`, never an averaged pass. | Only plans and deterministic assertion subsets may be byte-stability gates. Provider outputs, request IDs, timing, retries, usage, and cost remain immutable nondeterministic evidence. | Independence spans producer, QA actor, rubric author, provider/model family, credential/budget authority, and expected-answer access. Correlated majority voting is not independence. | Reimplement a minimal calibrated evidence contract; do not import a grader framework or grant a grader release/tool authority. | Trial count, human sample size, uncertainty, repeated-run variance, calls, tokens, and cost are measured under the later child contract. |
| 3. Agent trajectories, tool traces, side effects, sandbox, provenance | Work/event/artifact sources exist, but judge records do not bind the complete trajectory and side effects. | Inspect sample events/sandbox/limits; promptfoo tool sequence/args/count assertions; DeepEval expected tools. | Read-only evaluation consumes immutable #693/#694/#695 evidence. Active scenarios execute only through #694 in disposable capability-bounded workbenches. Missing source/cleanup evidence, forbidden effects, or authority mutation fail the run. | References include source generation, stable identity, and content digest. #709/#731 envelopes and #710 receipts provide idempotent resume; no competing event or trajectory ledger is created. | Candidate output/tool traces are untrusted data, never judge instructions. QA policy alone supplies expected answers, rubric, tool policy, and thresholds. Real-provider graders have cost/rate caps but no tool or release authority. | #696 owns the append-only QA authority; #706 closes readiness/quarantines on dependency failure; #722/#736 protect recovery and retention. | Query/index, cleanup, and active-scenario resource cost require later owner-authorized runtime validation; no build-server timing. |
| 4. Adversarial prompts, role drift, policy evasion, minimization | Fourth-wall patterns, drift heuristic, and security tests cover narrow classes. | promptfoo strategies/plugins and trace assertions; garak probes/detectors/calibration; PyRIT as secondary reference. | Every generated case retains generator revision, seed, parent, transforms, result, and minimized reproducer. Promotion requires review, provenance/license, deterministic capture, access classification, and expected policy oracle. | Discovery never overwrites canonical cases. One immutable finding may link 1:n to rotated hidden canaries and public regression fixtures. Aggregate scores cannot mask a failing critical role/project/model/language/surface slice. | Hidden canaries and full blocking corpora are encrypted/access-controlled and denied to candidate roles. Leakage marks affected cases contaminated, blocks their authority, and starts investigation/rotation. | Port algorithms and author Sentinel cases; do not copy corpora or plugin runtimes. | Corpus breadth, minimization yield, slice coverage, calls, tokens, and cost are later structural/cost experiments, not current benchmarks. |
| 5. Company E2E, release thresholds, baselines, flakes | #650/#696 specify independent QA and exact candidate lineage; implementation remains owned there. | No upstream owns Sentinel's customer, authority, release, or exact-digest semantics. Inspect failure states are a useful record pattern only. | Retry count/classes are predeclared and every attempt retained. A required deterministic failure followed by pass is `flaky_unresolved` until an authorized, expiring disposition and regression fixture exist. Harness error is distinct from product failure; quarantine cannot shrink required inventory. | Candidate, plan, fixture, environment, policy, source-evidence, deterministic-result, and final receipt digests are immutable. Stale, pruned, different-generation, or different-digest QA is rejected. | Separation of duties, authenticated customer actions, least privilege, holdout secrecy, rollback, retention, and audit are domain requirements. | Keep Sentinel. #696 owns release policy over #709/#710/#722/#706/#736 durability/recovery contracts; external tools are replaceable evidence producers only. | Product acceptance performance belongs to authorized M0 runtime issues, not this study. |

## One decision per mechanism

| Mechanism | Decision | Why this one | Rejected alternatives |
|---|---|---|---|
| 1. Dataset and reproducibility | **Port algorithm/contract** | Port Inspect record/failure concepts and lm-eval seed discipline into Sentinel-owned plan, dataset, deterministic-result, and operational-receipt schemas with explicit access/retention. | Adopt/Wrap would add large Python runtimes; Configure existing dependency cannot supply Sentinel lineage; Keep Sentinel unchanged leaves the schema and holdout gaps. |
| 2. Grading and calibration | **Reimplement minimal** | A small calibrated evidence contract preserves attributable model output, human labels, independence dimensions, uncertainty, parse failures, and disagreement without granting framework authority. | Adopt DeepEval/Inspect couples QA to provider frameworks; pure Keep Sentinel preserves uncalibrated one-shot judging; Patch upstream does not solve domain ownership. |
| 3. Trajectories and side effects | **Integrate** | Bind read-only QA and capability-bounded active scenarios to #693/#694/#695 sources using generation-bound IDs plus content digests and #709/#710 durability. | Reimplement creates a second truth store; porting upstream trace schemas loses Sentinel authority semantics; adopting tracing runtimes increases duplication. |
| 4. Adversarial regression | **Port algorithm/contract** | Port probe/strategy/reducer patterns and author a Sentinel-owned reviewed, access-controlled corpus with contamination and promotion rules. | Adopt promptfoo/garak/PyRIT creates a broad plugin/runtime boundary; copying corpora violates provenance discipline; Keep Sentinel unchanged leaves systematic coverage gaps. |
| 5. Product release and customer acceptance | **Keep Sentinel** | Exact candidate/plan/evidence digests, legal flake handling, separation of duty, customer action, retention, recovery, rollback, and closeout are Sentinel contracts owned by #696/#650. | External release authority is architecturally invalid; wrapping an OSS benchmark cannot create the missing company/customer semantics. |

No `Patch upstream` recommendation is justified. The accepted gaps are
Sentinel integration and authority gaps, not defects in an upstream project.

## M0 classification

| Capability | Classification | Owner | Rationale |
|---|---|---|---|
| Exact plan/candidate/dataset/evaluator/policy identity and typed case outcomes | `BLOCKS_M0` | #696 under #650; durable envelope #709/#731 | Independent QA cannot be fail-closed without knowing the canonical plan, exact evidence generation, and whether required missing/errored cases were rejected. |
| Read-only source evidence and capability-bounded active-scenario authority | `BLOCKS_M0` | #694 execution and #695 workflow, integrated by #696/#710 and traced by #693 | Product acceptance must prove authorized effects while preventing evaluators from mutating agreement, project, release, or customer authority. |
| QA durability, retention frontier, recovery, readiness, and quarantine | `BLOCKS_M0` | #696 with #709/#710/#722/#706/#736 | Release, rollback, audit, and customer retention cannot rely on prunable or unrecoverable QA evidence or fail-open readiness. |
| Release threshold, stale/different-digest rejection, legal flake state, customer acceptance | `BLOCKS_M0` | #696 and #650 | These are explicit M0 acceptance requirements; a later pass cannot erase a required deterministic failure. |
| Human-calibrated grader independence, disagreement, and judge-injection fixtures | `M0_HARDENING` | [#749](https://github.com/silentspike/project-sentinel/issues/749), child of #696 | Semantic quality hardens M0, but correlated graders and model votes cannot replace deterministic invariants. |
| Versioned hidden/adversarial corpus, contamination response, slice gates, and minimization | `M0_HARDENING` | [#750](https://github.com/silentspike/project-sentinel/issues/750), child of #696 with security review | A bounded reviewed set can harden M0; broader corpus growth remains staged and must preserve holdout secrecy. |
| Broad multi-model leaderboard and generalized benchmark catalog | `POST_M0` | #27 or a later approved successor | Comparative research value does not block one correct customer product path. |
| Direct integration of an OSS evaluation framework | `POST_M0` | #705 then #656 | Rejected unless a later necessity review clears the dependency and authority bar. |

## Proposed implementation contracts

The ORC approved the five mechanism directions in its review of PR #748 and
required the following concrete contracts before materialization. The schemas
below are Sentinel contracts, not copies of an upstream schema.

### Contract A: Sentinel QA evaluation record

**Proposed owner:** #696; blocks #650 acceptance.

#### Canonical schema set

All digests use canonical JSON with explicit schema/version tags and domain
separation. IDs alone are locators; every authoritative reference also carries
its source generation and content digest.

| Schema | Required fields | Digest and authority rule |
|---|---|---|
| `QaEvaluationPlanV1` | `plan_id`, `plan_generation`, `request_id`, `request_digest`; candidate artifact/source digests; agreement/project/work-item/acceptance-criterion IDs and digests; required/optional case inventory; fixture-set, evaluator-set, aggregation/release-policy, runner-binary, toolchain, sandbox, capability-profile, environment, and credential-policy digests; declared seeds; retry count and retryable classes; data-control policy revision | `EvaluationPlanDigest` covers every field except the locator `plan_id`. A changed candidate, requirement, case, policy, runner, toolchain, environment, or access rule creates a new plan generation and digest. |
| `QaDatasetCaseV1` | Case ID/revision; split `development`, `calibration`, hidden `holdout`, or `adversarial_canary`; required/optional class; role/project/model/language/surface slices; input and expected-oracle references/digests; oracle revision; source/provenance/license; data classification; allowed roles; creation, retirement, contamination, and supersession state | Case bytes and metadata are immutable per revision. Retirement/supersession is append-only. Hidden expected answers and canaries are not included in candidate-visible plan material. |
| `QaEvaluationRunReceiptV1` | Run ID/generation; plan/request digests; lifecycle state; `retry_of`, `supersedes`, and `superseded_by`; QA actor and executor IDs; durable event-envelope/generation IDs; admitted/started/finished timestamps; attempt IDs; harness outcome; cleanup receipt; aggregate case counts; release-gate receipt reference | This is an immutable operational receipt, intentionally not byte-stable across runs. A retry is a new generation linked to the failed run; final generations never reopen. |
| `QaCaseResultV1` | Run/plan/case IDs and digests; required flag; typed state `pass`, `fail`, `error`, `unscored`, `skipped`, `needs_human_review`, or `flaky_unresolved`; typed reason; assertion-result and grader-evidence references; immutable source-evidence tuples `(owner, type, id, generation, digest)`; slice values; attempt history; disposition reference | Missing, mutable, pruned, wrong-generation, or digest-mismatched evidence makes a required case `error`. Aggregation never converts absent evidence to pass. |
| `QaDeterministicAssertionResultV1` | Plan/case digest; assertion ID/revision; expected-oracle digest; canonical input/evidence digests; typed result; actual-value digest; failure details reference | `DeterministicResultDigest` is byte-stable for the same canonical plan, assertion, oracle, and source bytes. Operational timestamps and attempts are excluded, not normalized away. |
| `QaModelGradeEvidenceV1` | Run/case/plan digests; grader role; provider endpoint class and API version; requested and reported model ID; provider fingerprint when available; explicit `unknown`/`unavailable` identity fields; model family/version; system/rubric/prompt and structured-output-schema digests; temperature, top-p/top-k, max tokens, stop parameters, seed/support status; request/response IDs; attempt; raw-output reference/digest; parsed verdict/score/explanation reference; parse status/error; usage and cost | Immutable attributable evidence, never a deterministic-result digest. Unknown provider/model identity remains explicit and prevents a byte-reproducible-model claim. Raw output, timing, IDs, retries, usage, and cost are never normalized away. |
| `QaCalibrationReceiptV1` | Calibration-set and grader revisions; blinded human labels; at least two rater identities for ambiguous semantic cases; adjudicator and adjudication receipt; independence profile; sample/class counts; confusion matrix; false-pass/false-fail rates; inter-rater and grader/human agreement; repeated-run variance; uncertainty/confidence interval; unresolved classes | Immutable per calibration generation. Insufficient sample size, uncertainty, disagreement, or independence produces `needs_human_review` or blocks use as a required grader. |
| `QaFlakeDispositionV1` | Case/run/attempt digests; owner; product-vs-harness classification; reason; policy revision; created/expiry; linked defect and deterministic regression fixture; allowed action | Append-only. It never erases attempts or rewrites `flaky_unresolved`; the release gate may proceed only under a current authorized disposition and a passing required regression fixture. |
| `QaReleaseGateReceiptV1` | Candidate, plan, required-inventory, deterministic-result, model-evidence, calibration, source-evidence, flake-disposition, policy, release-manifest, and actor digests; final decision/reasons | #696 is the sole QA/release authority. The receipt is invalid for another candidate, generation, policy, evidence set, or release manifest. |

Data-control metadata on plans, cases, raw outputs, source references, and
receipts includes classification, encryption/key owner, access-control policy,
redaction policy/revision, retention class/frontier, audit actor/time/reason,
and deletion/retirement authority. Redaction creates a public view; it does not
alter the authoritative content digest.

#### Determinism boundaries

1. `EvaluationPlanDigest` is canonical and deterministic.
2. Only the deterministic subset of `QaDeterministicAssertionResultV1` is a
   byte-stability gate.
3. `QaEvaluationRunReceiptV1` preserves timestamps, generation, attempts, and
   operational outcomes and is not byte-stable between executions.
4. `QaModelGradeEvidenceV1` is probabilistic evidence. Provider outputs,
   request/response IDs, timing, usage, cost, and retry history remain intact.
5. A model score or majority vote cannot override a deterministic failure.

#### Run, retry, and flake state machines

| State | Legal next states | Rules |
|---|---|---|
| `planned` | `admitted`, `cancelled`, `superseded` | Admission verifies current plan/candidate/policy/source generations and required retention holds. |
| `admitted` | `running`, `harness_error`, `cancelled`, `quarantined` | Execution starts once under a durable #710 operation/reservation. |
| `running` | `needs_human_review`, `completed_pass`, `completed_fail`, `harness_error`, `quarantined` | Required missing/error/unscored/skipped evidence prevents `completed_pass`. |
| `needs_human_review` | `completed_pass`, `completed_fail`, `quarantined` | Requires an authenticated adjudication receipt; the evaluator cannot self-adjudicate. |
| `completed_pass`, `completed_fail`, `harness_error`, `cancelled`, `superseded`, `quarantined` | none | Terminal and immutable. Retry creates a new run generation linked by `retry_of`; it never reopens a receipt. |

Retry count and retryable harness/infrastructure error classes are fixed in the
plan before admission. Product assertion failure is not a retryable harness
error. Every attempt is retained. If a required deterministic case fails and a
later attempt passes, its aggregate state is `flaky_unresolved`; it blocks
release until `QaFlakeDispositionV1` names owner, reason, expiry, policy
revision, defect, and a passing deterministic regression fixture. Quarantine
cannot remove a required case or lower the required inventory digest.

#### Acceptance contract

1. Plan and deterministic-result digest golden tests reject every field
   omission, mutation, generation mismatch, and unstable canonical encoding.
2. Missing output, parse failure, stale candidate, changed policy, incomplete
   required inventory, unknown required identity, or source digest mismatch
   fails closed.
3. Restart resumes the same durable operation or creates a linked new
   generation without duplicate effective acceptance.
4. Release references the exact `QaReleaseGateReceiptV1`; another candidate,
   plan, evidence generation, or manifest is rejected.

### Contract B: Trajectory and side-effect conformance

**Proposed owners:** #694 for workbench/tool evidence, #695 for workflow
evidence, #693 for conformance-matrix traceability, and #696 for read-only
evaluation/release authority, with #709/#731 and #710 for durability.

#### Evaluation lanes

| Lane | Input and authority | Mandatory controls |
|---|---|---|
| `read_only_evidence` | Reads immutable #693/#694/#695 event, invocation, artifact, policy, approval, and completion evidence. It creates QA results only. | Source generation plus content digest; no workflow/artifact mutation; no network/tool authority; deterministic replay; missing/pruned evidence is an error. |
| `active_scenario` | Executes a declared case only through #694 in a disposable workbench. It may create isolated scenario artifacts/effects but never production or company authority. | Capability profile, denied production credentials, declared network/provider allowlist, process/filesystem/token/cost/rate limits, effect receipts, cleanup/rollback receipt, and #710 idempotent resume/unknown-outcome handling. |

Candidate output, tool traces, retrieved text, and generated artifacts are
untrusted data and are never concatenated as judge instructions. Expected
answers, rubrics, tool policy, release thresholds, and access decisions come
only from authenticated QA policy. Evaluators cannot mutate agreements,
projects, work items, approvals, release manifests, delivery, acceptance, or
customer authority. Real-provider graders have bounded credentials/budget but
no tool or release capability.

#### Acceptance contract

1. Missing, pruned, mutable, or differently-digested source evidence is an
   evaluation error.
2. A forbidden side effect fails even when the final textual answer is correct.
3. An expected side effect binds actor, authority generation, policy, target,
   idempotency key, effect receipt, and exact result digest.
4. Restart/replay resolves the same read-only evidence; an active scenario
   resumes/probes through #710 without duplicate effects.
5. Cleanup failure or retained capability makes the run `quarantined`, closes
   #706 readiness for that lane, and cannot yield release evidence.

### Contract C: Calibrated judge suite

**Materialized owner:** [#749](https://github.com/silentspike/project-sentinel/issues/749),
a quality-ready child under #696; `M0_HARDENING`.

#### Independence and calibration

- Build a reviewed fixture set for correctness, role adherence, judge prompt
  injection, malformed grader output, ambiguous cases, and known disagreements.
- Require deterministic assertions where an oracle exists; model grading covers
  only declared semantic questions.
- Record independence across candidate producer, QA actor, prompt/rubric
  author, provider, model family/version, credential/budget authority, and
  access to expected answers. Required dimensions are policy-versioned.
- Majority voting among correlated graders is not independence.
- Human-label ambiguous semantic cases with at least two raters, blinded where
  applicable, and retain a separate adjudication receipt.
- Report per-class confusion matrix, false-pass/false-fail rates, inter-rater
  agreement, grader/human agreement, repeated-run variance, sample size, and
  uncertainty. Preserve role/project/model/language/surface slices.
- Emit `needs_human_review` for unresolved disagreement, insufficient
  calibration, or missing required independence. Required independence failure
  closes the gate.

Threshold or rubric change requires a new immutable generation, pinned
before/after fixture comparison, uncertainty report, and owner approval.
Delimiter, metadata, indirect-prompt, and candidate-output injection tests must
prove the rubric and expected answer are not attacker-controlled.

### Contract D: Adversarial regression registry

**Materialized owner:** [#750](https://github.com/silentspike/project-sentinel/issues/750),
a quality-ready child under #696 with security review; `M0_HARDENING`.

#### Holdout, contamination, and promotion

- Create a Sentinel-authored registry of attack families, generator revisions,
  deterministic seeds, parent/minimized cases, expected policy outcome, and
  target surface.
- Separate development, calibration, hidden holdout, and adversarial-canary
  inventories with independent digests and access policies.
- Encrypt/access-control hidden inputs, expected answers, rubrics, canaries, and
  the complete blocking corpus. Candidate-producing actors, tools, prompts, and
  retrieved context cannot read them.
- Start with role drift, policy evasion, indirect/judge prompt injection, tool
  argument manipulation, sensitive-data extraction, and rubric exfiltration.
- Allow exploratory generation only in the Contract B active-scenario lane.
  Promotion requires provenance/license review, security review, deterministic
  minimization, expected-oracle revision, slice labels, and an immutable fixture.
- Mark leaked cases `contaminated`, remove their authority without deleting
  history, block affected holdout claims, investigate exposure, rotate a new
  hidden generation, and link supersession/retirement.
- Report critical role/project/model/language/surface slices separately.
  Aggregate averages cannot hide a critical-class regression.
- Keep upstream corpora out unless a separate record proves license,
  provenance, security handling, update owner, and necessity.

Remote generation, telemetry, and sharing are fail-closed off by default.
Generator nondeterminism cannot mutate an approved case. A minimized regression
must reproduce without a network dependency, and every family has an owner,
review cadence, retirement rule, and revisit condition.

### Contract E: M0 release-gate wiring

**Proposed owners:** #696 and #650.

- Gate the exact candidate, `EvaluationPlanDigest`, required-case inventory,
  source-evidence generation/digests, deterministic results, model evidence,
  calibration, flake dispositions, policy, and release manifest.
- Reject stale, missing, errored, unscored, skipped, self-approved,
  `needs_human_review`, `flaky_unresolved`, contaminated, pruned,
  wrong-generation, or differently digested required evidence.
- Prevent candidate producer, evaluator, QA actor, Release Management, and
  customer authorities from collapsing into an unauthorized identity.
- Bind QA approval, release, delivery, authenticated customer action,
  acceptance, rollback, closeout, and retention holds to one append-only
  lineage. No benchmark or model vote can waive a deterministic invariant.
- Make delivery/acceptance/retry idempotent under #709/#710 and recoverable under
  #722. Keep readiness closed/quarantined under #706 when authority or evidence
  dependencies are unavailable.

### Quality-ready materialization envelope

Contracts A, B, and E were appended to #696 without duplicating #693/#694/#695.
Contract C is materialized as #749 and Contract D as #750. They are registered
GitHub sub-issues of #696 because their human-governance and hostile-corpus
review units have different owners and rollout risk. No new epic was created.

| Contract | Dependencies and ordering | Negative criteria | Target-runtime tests | Issue-specific benchmark contract | Rollout and rollback | TOGAF delta |
|---|---|---|---|---|---|---|
| A. QA evaluation record | #693 contract first; #696 sole QA authority; #709/#731 envelope/outcome generation; #710 idempotent resume; #722 recovery; #706 readiness; #736 retention. | No normalized probabilistic evidence; no mutable/ID-only source ref; no required non-pass outcome; no unknown required identity; no producer self-approval. | Golden/mutation/state/retry/crash tests, then #696's owner-authorized target proves exact-candidate token-free QA and stale/missing/digest/generation substitution negatives. | Structural counts, canonical record size, event/effect counts, and owner-authorized runtime latency only; real-provider cost leg remains #650-owned. | Ship schema/read path, append-only producer, retention holds, then fail-closed consumer. Roll back implementation while retaining records; promotion stays closed if unavailable. | Target Cluster 08 must specify canonical QA plans/results, probabilistic evidence, durable lifecycle, and release linkage; no TOGAF edit in this worker PR. |
| B. Trajectory/side-effect conformance | #694 invocation/effects; #695 workflow; #693 traceability; #696 QA; #709/#710 durability; #706 cleanup quarantine. | No chat-only completion, copied ledger, missing authority/digest/cleanup, production credential, evaluator authority mutation, or candidate-controlled rubric. | Read-only replay plus active disposable-workbench escape/authorization/effect/idempotency/restart/cleanup tests on the owning issue's target. | Structural effect/reference counts mandatory; latency/resource figures only on the owner-authorized product target. | Add read-only projection first, then bounded scenario lane, then gate use. Roll back evaluators without mutating source records; quarantine failed cleanup. | Target architecture must distinguish read-only evidence evaluation from capability-bounded active scenarios and preserve one source of truth. |
| C. Calibrated judge suite | Requires A/B, #696 policy, two-rater/adjudication governance, independent credentials/budget, and #650 bounded provider proof. | No correlated-majority claim, producer/sole-judge, expected-answer leakage, swallowed parse/disagreement, insufficient sample/uncertainty, or silent threshold change. | Fake-grader deterministic outcome/injection tests; human-label/adjudication fixtures; repeated-run variance; owner-authorized bounded provider trials. | Per-class n/confusion/FPR/FNR/agreement/variance/uncertainty plus calls/tokens/cost; latency only on authorized target. | Advisory first; blocking only after calibrated classes and owner approval. Roll back rubric/threshold generation while retaining history. | Target Cluster 08/09 must specify grader attribution, independence, human calibration, uncertainty, and human-review state. |
| D. Adversarial registry | Requires A/B, #694 isolation, #696 QA, security approval, #736 retention; dependencies through #705 and upgrades through #656. | No copied/unlicensed corpus, candidate holdout access, plaintext uncontrolled canary, unreviewed promotion, hidden critical-slice failure, remote generation/telemetry by default, or silent contamination. | Registry/access/encryption/rotation/leakage/minimizer/slice/malformed tests, then bounded denied-egress corpus on the owner-authorized target. | Structural family/case/slice/minimization/detection and model-call/token/cost counts only; performance needs separate approved protocol. | Start with small reviewed non-blocking corpus, rotate/promote stable fixtures, retain contamination history, and roll back registry generation without deleting findings. | Target Cluster 08/09 must specify split secrecy, contamination/rotation, deterministic promotion, provenance, and slice gates. |
| E. M0 release gate | A/B plus #693/#694/#695/#696/#709/#710/#722/#706/#736; final #650 acceptance. | No stale/missing/self-approved/different-generation/different-digest/contaminated/pruned/`needs_human_review`/`flaky_unresolved` evidence; no benchmark/model override. | #696 failure/restart/recovery/retention/authority negatives and #650 exact-candidate delivery, acceptance, rollback rehearsal on its authorized target. | Inherit #696/#650 owner-authorized release-cycle protocol; no #717 timing or build-server claim. | Enable only after A/B durability and retention holds pass. Roll back gate code/config to prior verified revision, preserve failed evidence, and keep promotion closed. | Target architecture must link exact QA plan/evidence/retention generation to release, delivery, customer acceptance, rollback, and audit. |

### Durability, retention, and recovery contract

- #696 owns one append-only or generation-bound QA authority. QA plans, cases,
  receipts, dispositions, and release-gate decisions use #709/#731 versioned
  envelopes, expected-revision append, outbox/inbox outcomes, poison handling,
  and projection generations. No second event or trajectory store is allowed.
- #710 owns reservation, retry, effect receipt, unknown-outcome, idempotent
  resume, and crash semantics for active scenarios and provider calls.
- #722 whole-product recovery includes QA plans/receipts, fixture and calibration
  generations, evidence references, data-control metadata, retention holds, and
  release manifests. Restore validates all required digests before readiness.
- #706 closes readiness or quarantines the affected evaluation/release lane when
  required QA authority, storage generation, source evidence, cleanup, policy,
  or recovery validation is unavailable.
- #736 `RequiredQaEvidenceFrontier` prevents pruning source events, effects,
  artifacts, fixtures, calibration/holdout generations, QA receipts, and release
  manifests until release, rollback, audit, legal/customer retention, recovery,
  and supersession policies all permit retirement. Missing/unknown required
  frontier state means retain and fail closed.

Every materialized issue/body includes explicit AC mappings, terminal evidence
commands, evidence paths, rollout ordering, rollback owner, target class, and
claim boundary. The live readback is:

| Owner | Materialized contract | Final labels | Body SHA-256 | Fresh Issue Quality Gate |
|---|---|---|---|---|
| [#696](https://github.com/silentspike/project-sentinel/issues/696) | Contracts A, B, and E plus #709/#731, #710, #722, #706, and #736 dependencies | `status:ready`, `quality:ready` | `5921bd4f6c0d233a1fe87c6334fa0c43ac6b17d6ad98db84a37600fb5260b66f` | [PASS run 30427722618](https://github.com/silentspike/project-sentinel/actions/runs/30427722618) |
| [#749](https://github.com/silentspike/project-sentinel/issues/749) | Contract C: calibrated judge governance and human-labeled evaluation | `status:blocked`, `quality:ready` | `676dfc6db173bf7c5321b1e42c750307d86788177af071177677ad576b2aa475` | [PASS run 30427739097](https://github.com/silentspike/project-sentinel/actions/runs/30427739097) |
| [#750](https://github.com/silentspike/project-sentinel/issues/750) | Contract D: adversarial regression registry and hidden-corpus governance | `status:blocked`, `quality:ready` | `86f49f78268d629d1163013df4a46236fffcbcb664ec60705982689861ba8c49` | [PASS run 30427748271](https://github.com/silentspike/project-sentinel/actions/runs/30427748271) |

The [#717 materialization readback](https://github.com/silentspike/project-sentinel/issues/717#issuecomment-5113970964)
and [#659 reciprocal research link](https://github.com/silentspike/project-sentinel/issues/659#issuecomment-5113972630)
name all three implementation owners. The [#650 tracking
comment](https://github.com/silentspike/project-sentinel/issues/650#issuecomment-5113973391)
is explicitly non-blocking and changes no #650 scope, dependency, ordering,
runtime, or acceptance authority.

## Dependency and operations decision

- Add no dependency in #717.
- Keep #696 as the sole QA/release authority and use #709/#731, #710, #722,
  #706, and #736 for durability, execution, recovery, readiness/quarantine, and
  retention rather than introducing parallel infrastructure.
- If a future experiment proposes Inspect AI, promptfoo, DeepEval, garak,
  lm-evaluation-harness, or PyRIT, route necessity and ownership through #705
  before adding it.
- Route approved dependency upgrades through #656.
- Run any adversarial framework in an isolated QA environment with explicit
  network, token, process, filesystem, secret, and retention limits.
- Never run external evaluator plugins in the daemon, gateway, judge, release
  authority, or customer-acceptance process.
- Treat candidate output and traces as untrusted data. Keep expected answers,
  judge rubrics, release thresholds, hidden holdouts, and canaries behind
  authenticated QA policy and split-specific access controls.
- Never send Sentinel/customer traces to an upstream cloud or telemetry endpoint
  without a separate data-flow and operator approval.
- Do not claim runtime cost, throughput, or latency until an implementation
  issue defines an authorized target and measurement protocol.

## Acceptance-criteria mapping

| AC | Evidence in this study | Status |
|---|---|---|
| AC-1 | Pinned Sentinel source/test baseline, TOGAF references, owner and incident maps. | Satisfied |
| AC-2 | Eight-candidate reproducible inventory, explicit rubric, scores, shortlist, and rejections. | Satisfied |
| AC-3 | Five pinned deep reviews cover implementation, tests, failure modes, security, license, and operations. | Satisfied |
| AC-4 | Mechanism matrix covers correctness, failures, determinism, 1:n, security, maintenance, dependency/integration, and performance hypotheses. | Satisfied |
| AC-5 | Exactly one decision is selected for each of the five mechanisms with rejected alternatives; the ORC approved these directions subject to the bundled contract corrections now incorporated. | Satisfied |
| AC-6 | Contracts A-E define schemas, state machines, dependencies, negative criteria, target tests, measurement boundaries, rollout, rollback, and target-architecture deltas. #696 owns A/B/E; quality-ready #749/#750 own C/D; all three passed fresh Issue Quality Gates and are reciprocally linked to #717/#659, with non-blocking #650 tracking. | Satisfied |
| AC-7 | Every accepted capability is classified `BLOCKS_M0`, `M0_HARDENING`, or `POST_M0`. | Satisfied |
| AC-8 | This committed artifact is English, ASCII, public-safe, source-backed, and reproducible. | Satisfied |
| AC-N1 | No dependency is recommended merely because it exists upstream. | Satisfied |
| AC-N2 | License, provenance, hostile-data security, maintenance, and operations are reviewed before any port/copy recommendation. Corpora are explicitly excluded. | Satisfied |
| AC-N3 | Sentinel correctness is not inferred from service status, issue labels, or upstream tests. | Satisfied |
| AC-N4 | No runtime/build-server timing or benchmark claim is made. | Satisfied |
| AC-N5 | Every accepted gap maps to #650, #696, #693, #694, #695, #709/#731, #710, #722, #706, #736, #705, #656, #749, or #750; no accepted gap lacks a live owner. | Satisfied |

## Verification results and reproduction

The following fail-closed results were obtained from the final study content:

| Check | Result |
|---|---|
| Pinned source and line anchors | 80 `blob` links, 70 line-anchored, nine exact repository pins, Sentinel pin equals current `origin/main`, zero errors |
| Published URL reachability | 124 unique HTTPS URLs checked, 124 successful, zero failures |
| GitHub Markdown rendering | 309 rendered links, 18 tables, zero links without `href` |
| Study structure | Eight candidates, five deep reviews, five decisions, eight M0 classifications, five proposed contracts, 13 AC rows, and all 18 required contract terms; zero errors |
| Materialized contract readback | Three live owner bodies and digests, three final label sets, two registered sub-issues, three reciprocal comments, and three successful fresh Issue Quality Gate runs; zero errors |
| ASCII and public sanitization | ASCII decode passed; private path, private-network, host, user, and home-path scan returned zero findings |
| Spelling and Git whitespace | `typos docs/research/oss/judge-agent-evaluation.md` and `git diff --check` passed |

The source verifier parses every GitHub `blob/<sha>/<path>#Lx-Ly` link, maps it
to one of nine local repositories, reads the exact Git object rather than the
feature-branch checkout, and rejects an unknown repository, multiple pins for
one repository, missing object/path, or out-of-range line. It also requires the
Sentinel pin to equal current `origin/main` and every upstream pin to equal its
review checkout. The URL verifier extracts unique HTTPS targets and rejects
request errors or HTTP status 400 and above. The render check sends the document
to GitHub's GFM renderer and parses the returned HTML for tables and missing
link targets. The structure check rejects a candidate, deep review, mechanism
decision, M0 classification, proposed contract, or AC-count mismatch.

The seven-section PR contains the exact terminal commands and final-head outputs
for these checks plus `git rev-parse`, merge-base, changed-file scope, and
closing-issue readback. No Rust gate, runtime target, deployment, or benchmark
is part of this documentation-only change.
