# Candidate CI Contracts

Runtime target class: NONE
Deploy targets: none
Read-only targets: repository metadata only
Forbidden targets: production and cluster VMs
Benchmark targets: N/A
Rollback owner and method: integration owner; revert repository changes

## Routing And Merge

`route.py` inventories a complete Git comparison with NUL-delimited names.
Unknown inputs invalidate all gates. Only root Markdown and Markdown under
`docs/` may skip runtime gates. Classifier failure, empty inventory, missing
outputs, cancelled jobs and required-job skips cannot produce `ci-pass`.
Fork-triggered jobs use hosted runners regardless of `RUNNER_MODE`. Private
controller credentials, reporting keys and lab access are never workflow inputs
to fork jobs. Runner-group access policy must also exclude untrusted workflow
references: repository workflow edits alone cannot constrain a malicious fork's
changed workflow file. GitHub's fork-workflow approval controls remain required.

The full existing hosted matrix remains required. eBPF compiler and test failures
are fatal, including on hosted runners. These userspace feature gates do not
need host kernel headers or linux-tools; actual privileged kernel probes remain
a separate lab acceptance path, never inferred from hosted compilation.

Safe merge requires an independently reviewed full SHA, either as argument three
or `REVIEWED_HEAD_SHA`. For the existing Make target:

```sh
REVIEWED_HEAD_SHA=<full-reviewed-sha> make safe-merge PR=<number>
```

The script validates same-repository/main scope, issue readiness, current workflow
results and the latest `ci-pass` from GitHub Actions (application 15368), then
passes `--match-head-commit` to GitHub. It never bypasses branch protection. A SHA
argument binds the reviewed revision; it does not itself prove independent review.
The integration owner must retain the actual exact-head review evidence.

## Receipt V1

`candidate.py` validates an exact candidate envelope. Required gate names come
from a separately frozen qualification plan, never from the receipt itself.
`qualification-policy.json` is the full shadow matrix used by the reporter.
It must be reconciled with the actual archive inventory and applicable commands
before recording the first candidate. No fabricated successful gates are allowed.

Required fields:

- `schema_version`: integer 1; `repository`: `silentspike/project-sentinel`.
- `pr`: positive integer; `head_sha`, `tree_sha`, `base_sha`: full Git identities.
- `inputs`: `toolchain` 1.99.0, `profile` linux-gnu-sentinel, `image` the exact
  64-character Docker content identity from the audited Cargo Remote mapping,
  and `input_digest` covering the frozen source/build inputs.
- `gates`: exact unique required names, each with `source_digest` equal to the
  input digest, nonempty `command` argument list, integer `exit_code` zero,
  `counts` containing integer `passed` and `failed`, and an `evidence_sha` digest.
  Test/shard gates need positive passed counts; failed counts must be zero.
- `test_inventory_digest`: the archive lane's complete coverage inventory.
- `qualification_plan`: independently frozen applicability, bound to repository,
  PR/head/tree/base, source `input_digest`, `test_inventory_digest` and the archive
  `archive_plan_digest`. Its `owners` inventory includes all six archive owner
  groups, including empty groups. Each entry has exactly `owner`, boolean
  `required`, nonempty `reason`, `expected_count` and `inventory_id_digest`.
  The last two match the archive verifier's exact-ID owner inventory, never a
  regex/source count. Diagnostics or optional hardware contracts may be explicitly
  not applicable with reviewed rationale; this does not claim they passed.
  Required compiler-dependent tests need separate `rust-compiler-tests` gate
  evidence. `rust-doctests` and explicit eBPF feature gates remain independently
  required by the static policy; the applicability plan cannot omit those gates.
- `archive`: compact projection of the archive verifier's mechanism-only summary,
  including source/archive/manifest/plan/inventory hashes, original summary hash,
  `cargo_profile` ci (distinct from the builder profile), nextest 0.9.145,
  `expected_owners`, `observed_owners`, `owner_inventory`, pending non-sharded
  counts and verified shard totals. `aggregate_pass` must remain false. The
  controller runs archive verification against actual exact test IDs and JUnit
  before signing; a self-report from a test VM is not sufficient.
  Empty archive inventories, empty shard sets and zero observed totals are not
  qualification. A package-scoped pilot does not prove full workspace coverage.
- `shards`: the verifier's compact shards, exactly once in the envelope. They bind
  owner, nextest version, archive plan/inventory, original receipt/JUnit/log hashes,
  command and successful exit code. Partitions are contiguous and complete
  independently for lab-normal and lab-ignored, not in a shared global range.
  Required owners have exact positive per-shard counts and empty missing,
  duplicate and extra lists. Not-applicable owners have no executed shards and
  do not become implicit passes. Protected tests never become lab shards.
  A whole lab-ignored exclusion must also appear explicitly in the archive
  verifier's `not_applicable_owners` map with the unchanged inventory count and
  digest. It is honored only when the independently frozen plan agrees.
- `artifacts`: unique relative `path`, positive integer `size`, exact `sha256`
  and `build_origin` equal to the frozen head. No private paths or infrastructure
  details belong in public receipts. Artifact origins must come from retained
  build evidence; writing a receipt does not establish that a build occurred.

Example validation (repeat the gate argument for the complete frozen plan):

```sh
python3 scripts/ci/candidate.py verify --receipt receipt.json \
  --head <qualified-head> --base <qualified-base> --required-gate rust-tests \
  --plan independently-frozen-applicability.json
```

Plans use `canonical()` encoding from `candidate.py` (sorted compact ASCII JSON
with one trailing newline). The SHA256 of those canonical bytes is enrolled by
the controller as repository variable `SENTINEL_CI_PLAN_SHA256` after independent
review/freeze and before qualification. This pins one current aggregate candidate;
changing a receipt's applicability cannot change the trusted variable. The local
CLI accepts either a separately frozen `--plan` file or an independently enrolled
`--plan-sha256`. Neither may be derived from an unreviewed incoming receipt.

To construct the bounded envelope from retained verifier evidence:

```sh
python3 scripts/ci/candidate.py assemble --receipt draft-gates-and-artifacts.json \
  --archive-summary verified-archive-summary.json --plan independently-frozen-applicability.json \
  --head <qualified-head> --base <qualified-base> --required-gate rust-tests
```

Repeat every required static gate argument, as for verify. Assembly projects only
the summary's compact identity/count/digest fields and shard assertions, omits
large blocked-test lists and recipes, validates the complete envelope and rejects
payloads exceeding the reporter's 60000-character base64 limit. Full exact-ID and
JUnit evidence stays with the controller and is bound through retained hashes.

## Shadow Reporting

`candidate-report.yml` runs only by dispatch on protected main, on a hosted
runner. It executes trusted main scripts, never candidate code or arbitrary URLs.
Its `GITHUB_TOKEN` has repository `checks:write`, not infrastructure permissions.
A dedicated repository secret `SENTINEL_CI_RECEIPT_KEY` authenticates receipts;
the same key remains only on the trusted controller, never on test VMs/forks.
The ASCII key bytes are used exactly as provisioned, not hex-decoded or trimmed.

Dispatch inputs are `receipt_b64` (base64 of the exact public-safe JSON bytes,
at most 60000 encoded characters) and `signature` (hex HMAC-SHA256 over those
decoded bytes). Sign with the dedicated reporting key, not any GitHub token.
Dispatch `candidate-report.yml` with `--ref main` in this repository. The reporter
checks the trusted plan variable, current PR/head/base/tree and
schema/gates/coverage, then publishes
`candidate-qualification-shadow` on that head. Invalid authenticated qualification
publishes failure; invalid authentication or stale identity cannot publish success.
Receipt re-delivery updates the original same-application check. Neither the key,
raw commands nor private evidence logs are published in the check output.

This is shadow evidence, not required `ci-pass`. Keep old full gates until a real
candidate demonstrates equivalence and missing-shard, failed-gate, stale-head,
wrong-signature and fork negative dispatches have been verified. Local test
fixtures do not establish a live GitHub reporting result.

Primary API contracts:
- https://docs.github.com/en/actions/security-for-github-actions/security-guides/automatic-token-authentication
- https://docs.github.com/en/rest/checks/runs#create-a-check-run
- https://cli.github.com/manual/gh_pr_merge

## Promotion

Build once from the frozen independently reviewed head and preserve the original
build-input manifest, remote job receipt and artifact hashes. Do not rebuild or
relabel binaries with a later merge SHA. A promotion receipt records both source
and merge identities while leaving embedded source identities untouched.

```sh
python3 scripts/ci/candidate.py promote --receipt receipt.json \
  --head <qualified-head> --base <qualified-base> --required-gate rust-tests \
  --plan independently-frozen-applicability.json \
  --repo-root <repository> --artifact-root <artifact-directory> \
  --merge-sha <actual-merge>
```

Promotion checks source tree and merge tree equality, qualified base AND head
ancestry, artifact sizes, paths and hashes. Thus squash/rebase commits lacking
qualified-head ancestry cannot reuse this promotion path. Changed inputs require
requalification. The existing owner/mode-sensitive release package keeps its
original source `git_sha`; a promotion sidecar records the later merge, never a
rewritten release manifest. Reverify bytes under the deployment owner's lock
before installation. This verifier neither installs artifacts nor grants release
or customer acceptance authority.

## Focused Checks

```sh
TMPDIR=<owned-scratch> python3 -m unittest discover -s scripts/ci/tests -p 'test_*.py' -v
node --test scripts/ci/tests/test-report.cjs
actionlint -shellcheck= .github/workflows/ci.yml .github/workflows/candidate-report.yml
bash -n scripts/safe-merge.sh
```

No Rust compilation is required for these focused checks. Rust gates remain on
Cargo Remote through the persistent project queue. Full qualification, independent
review, actual GitHub dispatch and deployment acceptance belong to integration.
