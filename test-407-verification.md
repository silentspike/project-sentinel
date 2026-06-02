# Issue #407 Verification - Runtime Contract ADR

Issue: https://github.com/silentspike/project-sentinel/issues/407

## Scope

ADR/deviation documentation for the Nano-Container runtime contract. DEV-007
supersedes DEV-006 and records the maintainer decision: Project Sentinel uses a
runtime-agnostic CRI-style contract with plural runtimes and no fixed default.
Runtime choice is per workload.

## Issue Readiness Evidence

Command:

```bash
gh issue view 407 --json number,title,state,labels,url --jq '{number,title,state,labels:[.labels[].name],url}'
```

Observed:

```json
{"number":407,"state":"OPEN","labels":["type:docs","status:triage","quality:ready", "..."]}
```

## AC Matrix

| AC | Evidence | Status |
| --- | --- | --- |
| AC-1 | `docs/togaf-deviations-v22.md` contains DEV-007 with three considered runtime options and four runtime families. | PASS |
| AC-2 | DEV-007 records no fixed default runtime and per-workload runtime choice. | PASS |
| AC-3 | DEV-007 records the seven contract operations: spawn, exec, snapshot, restore, health, isolate, migrate. | PASS |
| AC-4 | DEV-006 remains historical and is explicitly superseded by DEV-007. | PASS |
| AC-5 | `docs/togaf-gap-v22.md` Cluster 12 no longer states a WASM default and links #397 plus #394/#406 cross-architecture gates. | PASS |

## Contract Check

Command:

```bash
python3 scripts/check-adr-runtime-contract.py docs/togaf-deviations-v22.md
```

Observed:

```text
PASS DEV-006 superseded
PASS DEV-007 active decision
PASS options considered
PASS contract operations
PASS runtime families
PASS epic and cross-architecture gates
PASS gap document Cluster 12
runtime contract ADR check passed
```

## Repository Gates

Command:

```bash
cargo remote -H root@10.0.0.155 -t /tmp/builds -c -- fmt --check
cargo remote -H root@10.0.0.155 -t /tmp/builds -c -- clippy --workspace --all-targets -- -D warnings
cargo remote -H root@10.0.0.155 -t /tmp/builds -c -- test --workspace
```

Observed:

```text
fmt --check: exit 0
clippy --workspace --all-targets -- -D warnings: Finished `dev` profile ... exit 0
test --workspace: all workspace unit, integration, and doc tests passed; exit 0
```

## Token Safety

Command:

```bash
ssh ubuntu@10.0.0.240 "systemctl list-units --all --type=service | grep -Ei 'cortex|gateway' || true"
```

Observed:

```text
sentinel-gateway.service loaded inactive dead Sentinel Cortex Gateway - LLM Pipeline + Synthesis Engine
```

Gateway was not started. No LLM calls were required for this docs/runtime-contract
run.

## Benchmarks

Runtime performance is not a gate for #407. The related runtime benchmark was
run on the deploy VM for #408-#411 and is recorded in the corresponding evidence
files.
