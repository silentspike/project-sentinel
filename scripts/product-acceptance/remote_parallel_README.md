# Remote Parallel Acceptance Tools

These tools prepare evidence; they do not deploy, issue company commands, call a
provider, create employee decisions, or close an issue. Execution order and
runtime authorization remain in the internal execution SSOT. Keep inputs,
credentials, measurements and screenshots private under the issue-owned
`/work/tmp/project-sentinel/` directory. Never publish production identifiers.

## Focused Checks

```sh
python3 -B -m unittest discover -s scripts/product-acceptance -p remote_parallel_test.py -v
node --test scripts/product-acceptance/remote_parallel_desktop_test.mjs
git diff --check
```

The tests are synthetic mechanism checks, not genuine product acceptance.
No Rust job or full-workspace gate is needed for these standalone tools.

## Read-Only Target Probe

Run `remote_parallel_probe.py` on the declared runtime target, not a builder or
agent host. It can be streamed over SSH into `sudo -n python3 -`, without
installing a file or modifying runtime configuration. Required arguments:

```text
--target-class SINGLE_NODE|LAB
--expected-hostname <private runtime host>
--manifest-sha256 <independently verified installed manifest hash>
--endpoint readiness|health|phases
--samples 100 --interval 0.2 --seconds 60 --timeout 10
```

The loopback API credential stays in target memory. Responses are bounded and
only numeric health summaries are retained; no customer or agent payloads are
exported. Redirects and proxy environment variables are disabled. `health` is
deliberately limited to ten reads to avoid repeatedly exercising expensive
production validation. HTTP timeouts and an overall wall deadline are bounded.
Partial samples, failed HTTP reads, sidecar errors, release changes and service
restarts produce `INCOMPLETE`, not a pass. Readiness and full health are different
workloads; neither is an end-to-end product benchmark.

`phases` makes no HTTP request. It inspects at most 2000 existing PID-scoped
journal entries over ten minutes, bounded to 4 MiB/25 seconds. It exports only
allowlisted static timing fields for provider, completion/adoption, context,
provider preparation, funding preparation and reconciliation. It includes explicit truncation state.
The metadata names match existing source instrumentation. An empty window is
not zero latency. Context/funding records without request IDs remain process
observations and must not be attributed to an original task by timestamp alone.
Several instrumentation paths log only slow spans at INFO; fast DEBUG spans can
be absent. Log-span ranks are conditional on that censoring, not full-workload
quantiles. Rejected enqueue waits are not provider HTTP or inference latency.

```sh
python3 -B scripts/product-acceptance/remote_parallel_report.py phases <private-phase-observation.json>
```

The report also accepts sanitized provider records with `request_id`, `phase`,
`elapsed`, `unit` (`us` or `ms`) and boolean `accepted`. Duplicate phases for one
request require explicit attempt reconciliation. Provider-facing `http` is not
pure remote inference time. `persistence`, completion `admission`, preparation
and source-context observations are separate, potentially nested spans. Never
sum these with `total`, subtract unrelated medians, or merge distinct requests.

Sidecars include CPU counters, RSS, per-service I/O, RAM/cache/swap, disk counters,
load and PSI. Disk series retain major/minor identity; do not add a disk and its
partitions as independent I/O. Guest `MemTotal` is not assigned balloon memory.
The registered lab controller's private `telemetry` action supplies timestamped
host reserve/load/PSI, concurrent guests, actual balloon bytes and assignment.
Unknown hypervisor values stay unknown. Sidecars do not establish causal timing
or workload equivalence by themselves.

## Matched Release Evidence

`remote_parallel_report.py benchmark baseline.json --candidate candidate.json`
requires two `kind: release_benchmark` records, each with a pinned
`manifest_sha256`, `release_stable: true` and at least 100 successful
`samples: [{ok: true, internal_ms: ...}]`. Optional `provider_ms` and
`end_to_end_ms` are measured fields, never inferred by subtraction. Do not feed
readiness observations or fixture results into this command.

Match these declared inputs exactly:

```text
runtime_target_class (LAB or SINGLE_NODE), host_fingerprint, hardware_digest,
dataset_digest, workload_digest, cache_state (warm, process_cold or os_cold),
balloon_bytes, concurrency, background_load_id, build_profile, sample_kind
```

Each record has `sidecars` containing at least two samples for `cpu`, `ram`,
`io`, `cache`, `balloon`, `concurrent_load`. All numeric balloon samples must
equal the declared assignment. These structural checks do not independently
authenticate the benchmark producer; the exact-head reviewer checks provenance.
Use release artifacts on the same registered lab slot, then separately qualify
the declared final single-node target. Record actual source, binary hash,
target/kernel/ABI, dataset digest, VM reset, workload parameters, warmup,
cache protocol, times and live telemetry in the private measurement bundle.
Never drop production caches or copy production data/identity into the lab.
Process-cold does not mean OS-cache-cold. Reverse trial order or use repeated
ABBA blocks to expose ordering effects; keep order/cold samples separate.

The output reports N and nearest-rank p50/p95/p99 (p99 is null below 100).
N >= 100 is a minimum here, not a universal confidence guarantee. Temporal
dependence, one tail observation, order and background activity can invalidate
strong conclusions even at higher N. `MATCHED_MEASUREMENTS` is not statistical
significance, an SLO pass, product acceptance or proof of fivefold acceleration.

## Native Comparison Probe

`remote_parallel_native.py` prepares an executable bounded ABBA comparison using
the existing ignored query-only Rust diagnostic, not a new product benchmark.
It never compiles, deploys, snapshots, repairs, writes a database or dispatches
a provider. Do not execute until the integrated measurement plan declares a
runtime target and supplies reviewed native release-test binaries plus frozen
input. On a lab target all data must be sanitized; no production database or
identity may be copied there. On the final single-node target use an approved
measurement window and retain separately owned private inputs.

Run with a private config file after placing this module and the probe module
together on the declared target. Config keys:

```text
runtime_target_class: LAB | SINGLE_NODE
expected_hostname, manifest_sha256 (installed runtime pin)
root: absolute owned input directory below /work/tmp/project-sentinel
baseline, candidate, image, metadata: each {file: relative name, sha256: digest}
abba_blocks: 1 to 3 (each block is baseline/candidate/candidate/baseline)
```

The image is frozen query-only workflow SQLite; metadata is the exact typed
`source`/`issuer` input required by the existing diagnostic. Artifact hashes,
not filenames or current branch names, select baseline/candidate executables.
The command is fixed to
`store::tests::diagnose_existing_expired_funding_source_read_only`,
`adaptive_health_inventory` mode, three same-process passes and one test thread.
It requires exactly one actual passing test, three valid selection diagnostics,
three ordered health passes, `total_changes=0` and `writes=0`. Zero matching
tests are failure. Child time/output are bounded; only owned children are killed
on failure and raw native output is not exported. Frozen inputs, runtime manifest
and service identity are rechecked between legs. Each child has exact wait4
CPU/peak-RSS/block-I/O metrics with concurrent target resource sidecars.

Report first process-cold pass separately from the two same-process warm passes;
do not claim OS-cache-cold after image-hash reads. Default ABBA yields only two
cold samples per executable: empirical medians are exploratory and p99 remains
null. Hypervisor balloon/background-load sidecars are still required for an
accepted matched comparison. A diagnostic win does not prove that reconciliation,
response adoption or full-product latency improved. Use passive post-rollout
phase observations to verify that the measured domain is the actual bottleneck.

## Shared Product Pilot Bundle

The product owner supplies a protected, read-only exporter. There is no generic
journal GET. Reuse original project/session lineage and resolve the independent
QA registration fresh; never infer it from an old agent ID. Existing read paths:

```text
GET /operator/workflow/projects?project_id=<project>         (operator)
GET /company/delivery/lineage?tenant_id=<tenant>&project_id=<project> (release manager)
GET /api/v1/delivery/lineage?tenant_id=<tenant>&project_id=<project> (dashboard session)
```

Run `remote_parallel_report.py pilot bundle.json`. Bundle schema:

```text
schema: 1
mode: fixture | genuine_internal | external_customer
project_id, developer_id (integer), qa_id (integer, different developer)
qa_registration: {file: relative evidence path, sha256: digest}
records: exactly developer, qa_negative, correction, qa_positive, release, delivery
```

Every record has `stage`, `project_id`, strictly increasing durable `sequence`,
integer `revision`, `artifact_sha256`, and distinct `evidence: {file, sha256}`.
Developer/correction and QA decisions have their corresponding `actor_id`.
For non-fixtures they also require `decision_origin: provider_completion`,
`request_id` and `provider_completion: {file, sha256}`. Negative QA supplies
`report_digest`; correction supplies the same `qa_report_digest` and a newer
revision/different artifact. Positive QA, release and delivery must all bind that
corrected revision/artifact. Release requires `authority: release_manager`;
delivery `release_digest` must equal the release evidence hash. Customer mode
additionally requires separate `customer_agreement` and `customer_acceptance`
file/hash references. All files are bounded, digest checked and confined to the
bundle root. Evidence files can be protected redacted exports; retain their
authenticated original-source bindings separately for independent review.

The reviewer must inspect the actual typed work/output/gate receipts, negative
QA report/blocker, archived review, work-correction revision, completed journal,
Workbench package/receipt and provider reservation/outbox/usage relationship.
Verify actual model results rather than trusting exporter labels. The validator
checks supplied bindings, not cryptographic origin or payload semantics, and
always leaves product acceptance requiring independent runtime review. Fixture
mode always reports `FIXTURE_ONLY`; an internal developer proof is not external
customer acceptance. Missing records block the chain, not authorize retries.

## Desktop Journey

Install the exact Playwright version resolved by the repository's frozen lockfile
in an owned tools directory. Use its matching browser revision; an older browser
cache is not proof of compatibility. Record any platform fallback explicitly.
The runner takes a private JSON config, new output directory, and absolute
pinned Playwright module path:

```text
node remote_parallel_desktop.mjs <private-config.json> <owned-output> <playwright-module>
config: runtime_target_class, origin (HTTPS), ssh_target, manifest_sha256,
        certificate_file (public PEM), credential_env_file (protected dashboard env),
        tenant_id, project_id
```

It reads only the dashboard login key and public certificate into memory through
SSH, checks the release pin, and uses that certificate as the sole TLS trust
anchor with chain, validity and hostname validation enabled. It also compares
the exact TLS peer certificate, uses a Chromium SPKI-specific exception, and
compares the `/api/cert-hash` API with that exact certificate. The HTTPS origin
must match a certificate DNS/IP subject alternative name, including when using
an SSH tunnel; a mismatch fails closed. No global TLS bypass, credential file,
storage state, trace or
secret output is created. Normal login and ticket POSTs are allowed; all other
non-GET requests and cross-origin requests are blocked. No editor save, chat,
funding, provider or workflow command is clicked.

Desktop viewport is 1440x1000. It opens the actual Delivery view, observes lineage
HTTP state, node/digest/authority counts, invalid lineage, page errors and document
overflow, and hashes private screenshots. A 404 is `INACCESSIBLE_NOT_DELIVERED`,
never successful delivery. Even visible lineage is not full product acceptance.
A TCP tunnel cannot forward UDP WebTransport: live topics remain `NOT_VERIFIED`.

## Three Accepted Blocks

`remote_parallel_report.py cycles bundle.json` accepts exactly three distinct
externally verified genuine product blocks, not infrastructure tasks, source
subtasks or fixtures. Each block records `id`, `mode: genuine_product`,
`accepted: true`, `acceptance: {file, sha256}`, `started_ms`, `accepted_ms`,
`baseline_elapsed_ms`, `baseline_evidence: {file, sha256}`, and matching
`comparison_key`/`baseline_comparison_key`. Baselines must be measured and
comparable, not estimated by agent count. Each `spans` entry has `category`,
`start_ms`, `end_ms`, within the end-to-end interval. Categories are `wait`,
`integration`, `build`, `test`, `deploy`, `rework`; absent categories require
explicit `not_applicable`. Preserve source/job/review/deploy identifiers in
private supporting evidence, including transport/capacity waits and retries.

The report computes per-category and overall interval unions, elapsed time and
baseline/actual ratios. Overlapping waits/builds/tests are never added as total
wall time. Caller-supplied acceptance is not authenticated by this analyzer.
Until three useful product blocks are actually accepted, the throughput target
remains pending. Do not label this harness or a fixture as an accepted block.
