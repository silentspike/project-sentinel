# OSS Judge, Agent-Evaluation, and Adversarial-Testing Study

Status: research complete, implementation contracts proposed for ORC review
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
| Architecture SSOT | The guide names MARBLE as the multi-agent evaluation basis ([Cluster 09](https://github.com/silentspike/project-sentinel/blob/55ace5371a64d4369dccf7aea13ceb32ae441891/docs/architecture/togaf-architecture-guide.html#L2048-L2055)) and describes the judge/nightrun path ([Cluster 08](https://github.com/silentspike/project-sentinel/blob/55ace5371a64d4369dccf7aea13ceb32ae441891/docs/architecture/togaf-architecture-guide.html#L1938-L1950)). | The architecture documents current mechanisms, but does not define a versioned general-purpose evaluation record or calibrated release threshold. |

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
| 1. Dataset, fixtures, scoring, reproducibility | Heuristic scores and observatory records; no versioned general eval record. | Inspect versioned logs and typed sample errors; lm-eval seeds/cache/task schema. | A case must be `pass`, `fail`, `error`, `unscored`, or `skipped`; missing/parse/cache/revision mismatch cannot pass. | Pin fixture digest, evaluator revision, model/provider revision, all seeds, candidate digest, and source refs. Reuse source refs 1:n. | Fixture inputs may include customer data; redact or bind access without weakening evidence. | Small Sentinel-owned schema avoids Python/Node runtime ownership. | Record volume and scoring cost require later authorized measurement; no estimate is accepted here. |
| 2. Deterministic and model-graded scoring, calibration, disagreement | Fixed heuristics plus one-shot LLM judges; no calibration/disagreement record. | Inspect explicit unscored parse failures and multi-model vote; DeepEval rubric shape; garak calibration metadata. | Deterministic assertions run first. Model graders emit independent evidence; disagreement is preserved and cannot silently average to pass. | Deterministic checks are replayable. Model grades are reproducible evidence records, not deterministic truth. | Grader prompt injection, data exfiltration, shared-model bias, and token cost require isolated roles and budgets. | Reimplement minimal fields; do not import a grader framework. | Number of graders and retries is a cost hypothesis to tune only with a pinned corpus. |
| 3. Agent trajectories, tool traces, side effects, sandbox, provenance | Work/event/artifact sources exist, but judge records do not bind the complete trajectory and side effects. | Inspect sample events/sandbox/limits; promptfoo tool sequence/args/count assertions; DeepEval expected tools. | Evaluate expected and forbidden effects, order, authorization, artifact digest, error spans, and rollback evidence. Missing source evidence is an error. | Store immutable references to authoritative event/artifact rows. Never copy a competing trajectory ledger. | Tool output is untrusted; sandbox identity, policy revision, secrets exposure, and network effects must be recorded. | Integrate #694 execution and #695 workflow evidence into #696 under the #693 conformance contract rather than add another tracer. | Query/index cost requires later runtime validation; no build-server timing. |
| 4. Adversarial prompts, role drift, policy evasion, minimization | Fourth-wall patterns, drift heuristic, and security tests cover narrow classes. | promptfoo strategies/plugins and trace assertions; garak probes/detectors/calibration; PyRIT as secondary reference. | Every generated case retains generator revision, seed, parent case, transforms, detector result, and minimized reproducer. Nondeterministic generation cannot overwrite the canonical regression case. | Generated discoveries become reviewed, versioned deterministic fixtures. One finding may link to many runs 1:n. | Run only in isolated QA lanes with remote generation/telemetry off unless separately approved. Treat corpora as hostile content. | Port algorithms and author Sentinel cases; do not copy corpora or plugin runtimes. | Corpus breadth versus cost is a later security-test experiment, not a current benchmark. |
| 5. Company E2E, release thresholds, baselines, flakes | #650/#696 specify independent QA and exact candidate lineage; implementation remains owned there. | No upstream owns Sentinel's customer, authority, release, or exact-digest semantics. Inspect failure states are a useful record pattern only. | Deterministic product invariants and exact digest are mandatory. Model metrics can harden but cannot waive an invariant. Flakes remain failed/unresolved until classified with retained attempts. | Candidate, fixture, environment, policy, and result digests are immutable. Stale or different-digest QA is rejected. | Separation of duties, authenticated customer actions, least privilege, rollback, and evidence retention are domain requirements. | Keep Sentinel. External tools may run as replaceable isolated producers of evidence, never release authority. | Product acceptance performance belongs to authorized M0 runtime issues, not this study. |

## One decision per mechanism

| Mechanism | Decision | Why this one | Rejected alternatives |
|---|---|---|---|
| 1. Dataset and reproducibility | **Port algorithm/contract** | Port the Inspect record/failure concepts and lm-eval seed completeness into a compact Sentinel-owned schema. | Adopt/Wrap would add large Python runtimes; Configure existing dependency cannot supply Sentinel lineage; Keep Sentinel unchanged leaves the schema gap. |
| 2. Grading and calibration | **Reimplement minimal** | A small composite scorer contract can preserve deterministic results, grader evidence, parse failures, calibration, and disagreement without framework authority. | Adopt DeepEval/Inspect couples QA to provider frameworks; pure Keep Sentinel preserves uncalibrated one-shot judging; Patch upstream does not solve domain ownership. |
| 3. Trajectories and side effects | **Integrate** | Bind QA records to existing #693/#694 events, artifacts, sandbox and policy evidence with immutable references. | Reimplement creates a second truth store; Porting upstream trace schemas loses Sentinel authority semantics; adopting tracing runtimes increases duplication. |
| 4. Adversarial regression | **Port algorithm/contract** | Port probe/strategy/reducer patterns and author a Sentinel-owned reviewed corpus with explicit provenance. | Adopt promptfoo/garak/PyRIT creates a broad plugin/runtime boundary; copying corpora violates provenance discipline; Keep Sentinel unchanged leaves systematic coverage gaps. |
| 5. Product release and customer acceptance | **Keep Sentinel** | Exact candidate digest, separation of duty, customer action, rollback, and closeout are Sentinel product contracts already owned by #650/#696. | External release authority is architecturally invalid; wrapping an OSS benchmark cannot create the missing company/customer semantics. |

No `Patch upstream` recommendation is justified. The accepted gaps are
Sentinel integration and authority gaps, not defects in an upstream project.

## M0 classification

| Capability | Classification | Owner | Rationale |
|---|---|---|---|
| Exact candidate/fixture/evaluator identity and typed case outcomes | `BLOCKS_M0` | #696 under #650 | Independent QA cannot be fail-closed without knowing exactly what ran and whether missing/errored cases were rejected. |
| Trajectory, side-effect, artifact, and policy evidence references | `BLOCKS_M0` | #694 execution and #695 workflow owners, integrated by #696 and traced by #693 | Product acceptance must prove the delivered artifact came from authorized work and that forbidden effects did not occur. |
| Release threshold, stale/different-digest rejection, flake retention, customer acceptance | `BLOCKS_M0` | #696 and #650 | These are already explicit M0 acceptance requirements. |
| Calibrated multi-grader disagreement and judge-injection fixtures | `M0_HARDENING` | #696 | Useful for semantic quality, but must not replace deterministic M0 invariants. |
| Versioned adversarial regression corpus and deterministic minimization | `M0_HARDENING` | #696 with security review | Important hardening against role drift and policy evasion; initial M0 may use a bounded reviewed set. |
| Broad multi-model leaderboard and generalized benchmark catalog | `POST_M0` | #27 or a later approved successor | Comparative research value does not block one correct customer product path. |
| Direct integration of an OSS evaluation framework | `POST_M0` | #705 then #656 | Rejected unless a later necessity review clears the dependency and authority bar. |

## Proposed implementation contracts

These contracts are ready for ORC review. Per the #717 assignment, this worker
does not create implementation issues before the synthesis is reviewed.
Materialization must reuse or amend the listed owners rather than create
overlapping epics.

### Contract A: Sentinel QA evaluation record

**Proposed owner:** #696; blocks #650 acceptance.

**Scope**

- Add a versioned `QaEvaluationRecordV1` owned by the QA/release domain.
- Record candidate digest, fixture-set digest, evaluator build/revision, policy
  revision, environment identity, seed set, start/end, limits, and producer.
- Record each case as exactly one of `pass`, `fail`, `error`, `unscored`, or
  `skipped`, with typed reason and retained evidence references.
- Record deterministic assertion results separately from model-grader results.
- For every grader: model/provider revision, role, prompt/rubric digest, raw
  structured verdict, parse status, score, explanation reference, token/cost
  accounting, and attempt number.
- Preserve disagreement. A configured required case with missing, errored,
  skipped, or unscored evidence fails the gate.

**Acceptance contract**

1. Same fixture/evaluator/candidate/seed inputs produce byte-stable deterministic
   records after timestamp normalization.
2. Missing grader output, parse failure, stale candidate digest, changed policy,
   or incomplete required cases fail closed.
3. Model-only scores cannot waive a failed deterministic invariant.
4. Restart resumes or supersedes an incomplete run without duplicating an
   accepted result.
5. The release decision references the exact immutable QA record digest.

### Contract B: Trajectory and side-effect conformance

**Proposed owners:** #694 for workbench/tool evidence, #695 for workflow
evidence, #693 for conformance-matrix traceability, and #696 for read-only
evaluation and release use.

**Scope**

- Define expected, allowed, and forbidden tool calls, artifact mutations,
  network/process effects, approvals, and terminal work states.
- Resolve source evidence by immutable event/artifact/policy identifiers.
- Evaluate sequence, partial/exact arguments, count bounds, authorization,
  errors, rollback, and resulting artifact digest.
- Store references 1:n; do not copy source events or create another write path.

**Acceptance contract**

1. Missing, pruned, mutable, or differently-digested source evidence is an
   evaluation error.
2. A forbidden side effect fails even when the final textual answer is correct.
3. An expected side effect must bind to the authorized actor, policy revision,
   target, and exact result digest.
4. Restart and replay resolve the same evidence set and case result.

### Contract C: Calibrated judge suite

**Proposed owner:** #696.

**Scope**

- Build a reviewed fixture set for correctness, role adherence, judge prompt
  injection, malformed grader output, ambiguous cases, and known disagreements.
- Require deterministic assertions where an oracle exists.
- Run at least one primary and one independent audit grader for designated
  semantic cases; retain both verdicts instead of only a majority score.
- Version thresholds and calibration summaries against fixture and grader
  revisions.

**Acceptance contract**

1. Calibration reports confusion/disagreement by case class and never hides
   parse/error outcomes.
2. The same model cannot silently act as producer and sole judge.
3. Delimiter and metadata injection fixtures cannot alter the rubric or expected
   output contract.
4. Threshold change requires a pinned before/after fixture comparison and owner
   approval.

### Contract D: Adversarial regression registry

**Proposed owner:** #696 with the security owner; `M0_HARDENING`.

**Scope**

- Create a Sentinel-authored registry of attack families, generator revisions,
  deterministic seeds, parent/minimized cases, expected policy outcome, and
  target surface.
- Start with role drift, policy evasion, indirect prompt injection, tool
  argument manipulation, sensitive-data extraction, and judge injection.
- Allow exploratory generation only in an isolated lane. Promote a discovery
  into the release suite only after review and deterministic fixture capture.
- Keep upstream corpora out unless a separate record proves license,
  provenance, security handling, update owner, and necessity.

**Acceptance contract**

1. A minimized regression case reproduces the same policy failure without a
   network dependency.
2. Generator nondeterminism cannot mutate an already-approved canonical case.
3. Remote generation, telemetry, or sharing is fail-closed off by default.
4. Every accepted family has a named owner and revisit condition.

### Contract E: M0 release-gate wiring

**Proposed owners:** #696 and #650.

**Scope**

- Gate only the exact candidate digest under review.
- Require all `BLOCKS_M0` deterministic cases and required evidence producers.
- Treat stale, missing, errored, unscored, skipped, self-approved, or
  different-digest QA as rejection.
- Retain all attempts for a flaky case; do not convert a later pass into a clean
  history.
- Bind QA approval, release manifest, delivery, authenticated customer action,
  acceptance, rollback, and closeout to one lineage.

**Acceptance contract**

1. Independent QA cannot approve its own candidate-producing action.
2. A release cannot substitute a benchmark score for a failed product
   invariant.
3. Repeated delivery and acceptance commands are idempotent across restart.
4. The customer can identify the exact accepted artifact and evidence digest.

### Quality-ready materialization envelope

ORC should first apply Contracts A and E to #696, Contract B to the existing
#694/#695/#696 boundaries, and the traceability deltas to #693. Contracts C and
D should become ordered #696 children only if ORC confirms that adding them to
#696 would make its implementation/review unit unsafe. No new epic is needed.

Suggested labels for any approved child are `type:feature`, `comp:inference`,
`comp:runtime`, `prio:critical` for a `BLOCKS_M0` child or `prio:high` for
`M0_HARDENING`, `scope:full`, `quality:ready`, and the appropriate size label.
The live label set remains ORC-owned.

| Contract | Dependencies and ordering | Negative criteria | Target-runtime tests | Issue-specific benchmark contract | Rollout and rollback | TOGAF delta |
|---|---|---|---|---|---|---|
| A. QA evaluation record | #693 contract first; consume #694/#695 evidence; integrate in #696 before #650 final acceptance. No new OSS dependency. | No required missing/error/unscored/skipped case may pass; no mutable or different-digest source ref; no model-only override; no producer self-approval. | Unit/golden/failure-injection/restart tests first. Then inherit #696's owner-authorized product-runtime snapshot/deploy discipline and prove an exact-candidate token-free QA run plus negative stale/missing/digest-substitution cases. #717 itself performs none of these actions. | Correctness issue: structural counts by outcome/evidence type and record size are required. If latency is measured, use only #696's owner-authorized product-runtime protocol; never build-server timing. The bounded real-provider cost leg remains #650-owned. | Ship schema and read path, then producer, then fail-closed release consumer. Gate activation waits for backfill/migration and negative tests. Roll back by reverting the implementation PR/schema reader while retaining append-only records; if required QA is unavailable, promotion stays closed. | Main-session handoff for Cluster 08 Judge and the M0 QA/release contract after runtime verification; do not edit TOGAF in the worker PR. |
| B. Trajectory/side-effect conformance | #694 must expose invocation/tool/artifact evidence; #695 workflow/handoff evidence; #696 consumes; #693 matrix traces ACs. | No chat-only completion; no copied second event ledger; no absent authorization/policy identity; no textual success may hide a forbidden effect. | Use #694/#695 unit, escape, authorization, idempotency, restart, and work-journey tests. On their owner-authorized product-runtime snapshot, prove expected and forbidden effects and byte-identical source-reference resolution across restart. | Inherit #694/#695 authorized runtime protocols: structural effect/reference counts are mandatory; any latency/resource figures come only from owner-authorized product-runtime sidecars and are not part of #717. | Add read-only evidence projection, then assertions, then #696 gate use. Roll back the evaluation projection/assertions without mutating authoritative #694/#695 records; promotion remains fail-closed when the required evaluator is absent. | Main-session handoff for tool execution, workflow authority, and QA evidence arrows in the target architecture after each owner verifies its runtime path. |
| C. Calibrated judge suite | Requires Contract A, independent grader identities, #696 policy, and #650 for the bounded real-provider acceptance leg. | No same-model producer/sole-judge path; no swallowed parse error; no hidden disagreement; no threshold change without fixture delta; no external prompt/trace upload by default. | Fake-grader deterministic tests cover all outcomes and injection cases. On #696's owner-authorized product runtime target, run the pinned suite with model calls token-gated; reserve the capped real-provider independence proof for #650. | Report fixture-class counts, confusion/disagreement, parse/error rate, grader calls, and token/cost totals. Latency is optional and valid only on the authorized product target. No upstream/build-server benchmark claim. | Start advisory and retain all verdicts; promote only designated calibrated cases to blocking after owner approval. Roll back a bad rubric/threshold to its prior immutable revision; never rewrite historical verdicts. | Main-session handoff for Cluster 08 judge calibration/disagreement and Cluster 09 evaluation evidence after the suite is verified. |
| D. Adversarial registry | Requires Contract A, #694 sandbox/security boundaries, #696 QA ownership, and security-owner approval. Dependency proposals route #705; upgrades route #656. | No copied corpus without provenance/license review; no network/telemetry/remote generation by default; no unreviewed generated case in the blocking suite; no hostile payload in production authority paths. | Deterministic unit tests for registry, transforms, seeds, minimizer, policy verdicts, and malformed/hostile records. On an issue-specific owner-authorized product-runtime snapshot only after #694 isolation is verified, run the bounded canonical corpus with denied egress and stability/security readback. | N/A for product performance. Report structural family/case/minimization/detection counts and model-call/token/cost counts only. A future performance claim needs a separately approved runtime protocol. | Begin non-blocking with a small reviewed corpus, then promote individual stable cases. Roll back by disabling/reverting the offending registry revision while retaining findings; required previously approved cases remain blocking. | Main-session handoff for Cluster 08 adversarial judge coverage and Cluster 09 security-evaluation basis after bounded live verification. |
| E. M0 release gate | Contract A plus #693/#694/#695/#696; final acceptance #650. | No stale/missing/self-approved/different-digest QA; no later flaky pass erases earlier attempts; no benchmark score waives a product invariant; no unauthenticated customer acceptance. | Inherit #696 AC-3/4/12/13 and #650 final acceptance on the owner-authorized product runtime target: negative promotion/delivery attempts, restart at each boundary, functioning release, explicit acceptance, and rollback rehearsal. | Inherit #696's owner-authorized product-runtime protocol for at least 20 token-free release cycles with p50/p95/max and sidecars; the capped real-provider leg and final product claims remain #650. No #717 timing. | Enable only after A/B required evidence is complete. Roll back release-gate code/config to the previous verified revision and activate the previous approved release; preserve failed release/evidence and keep promotion closed during recovery. | Main-session handoff for the customer-to-QA-to-release-to-delivery-to-acceptance chain after #696/#650 verification. |

Every materialized issue/body must include explicit AC mappings, terminal
evidence commands, evidence paths, rollout ordering, rollback owner, target
class, and claim boundary. Reciprocal links to #717 and #659 are required only
after ORC approves this synthesis.

## Dependency and operations decision

- Add no dependency in #717.
- If a future experiment proposes Inspect AI, promptfoo, DeepEval, garak,
  lm-evaluation-harness, or PyRIT, route necessity and ownership through #705
  before adding it.
- Route approved dependency upgrades through #656.
- Run any adversarial framework in an isolated QA environment with explicit
  network, token, process, filesystem, secret, and retention limits.
- Never run external evaluator plugins in the daemon, gateway, judge, release
  authority, or customer-acceptance process.
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
| AC-5 | Exactly one decision is selected for each of the five mechanisms with rejected alternatives. | Satisfied |
| AC-6 | Contracts A-E are implementation-ready and mapped to existing owners. Live issue materialization is intentionally ORC-gated by the assignment. | Satisfied at research-review boundary |
| AC-7 | Every accepted capability is classified `BLOCKS_M0`, `M0_HARDENING`, or `POST_M0`. | Satisfied |
| AC-8 | This committed artifact is English, ASCII, public-safe, source-backed, and reproducible. | Satisfied |
| AC-N1 | No dependency is recommended merely because it exists upstream. | Satisfied |
| AC-N2 | License, provenance, hostile-data security, maintenance, and operations are reviewed before any port/copy recommendation. Corpora are explicitly excluded. | Satisfied |
| AC-N3 | Sentinel correctness is not inferred from service status, issue labels, or upstream tests. | Satisfied |
| AC-N4 | No runtime/build-server timing or benchmark claim is made. | Satisfied |
| AC-N5 | Every accepted gap maps to #650, #696, #693, #694, #695, #705, or #656; proposed contracts name the owner. | Satisfied |

## Verification results and reproduction

The following fail-closed results were obtained from the final study content:

| Check | Result |
|---|---|
| Pinned source and line anchors | 80 `blob` links, 70 line-anchored, nine exact repository pins, zero errors |
| Published URL reachability | 110 unique HTTPS URLs checked, 110 successful, zero failures |
| GitHub Markdown rendering | 214 rendered links, 14 tables, zero links without `href` |
| Study structure | Eight candidates, five deep reviews, five decisions, seven M0 classifications, five proposed contracts, and 13 AC rows; zero errors |
| ASCII and public sanitization | ASCII decode passed; private path, private-network, host, user, and home-path scan returned zero findings |
| Spelling and Git whitespace | `typos docs/research/oss/judge-agent-evaluation.md` and `git diff --check` passed |

The source verifier parses every GitHub `blob/<sha>/<path>#Lx-Ly` link, maps it
to one of the nine local exact-commit checkouts, verifies checkout SHA and file
existence, and rejects an out-of-range line. The URL verifier extracts unique
HTTPS targets and rejects request errors or HTTP status 400 and above. The
render check sends the document to GitHub's GFM renderer and parses the returned
HTML for tables and missing link targets. The structure check rejects a
candidate, deep review, mechanism decision, M0 classification, proposed
contract, or AC-count mismatch.

The seven-section PR contains the exact terminal commands and final-head outputs
for these checks plus `git rev-parse`, merge-base, changed-file scope, and
closing-issue readback. No Rust gate, runtime target, deployment, or benchmark
is part of this documentation-only change.
