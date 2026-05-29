# Issue #391 Verification

Issue: https://github.com/silentspike/project-sentinel/issues/391

## Scope

Prompt-Injection Defense for the Go Cortex Gateway: pro-agent capability
definitions, server-side action validation, audit events for rejected actions,
and injection/legitimate-action regression tests. Gateway must remain inactive
for token-safe verification; no real LLM calls are required.

## Issue Readiness Evidence

Command:

```bash
gh issue view 391 --json labels,body,state --jq '{state,labels:[.labels[].name], hasBenchmarks:(.body|contains("## Benchmarks")), hasReady:([.labels[].name]|index("quality:ready")!=null), hasNeedsSpec:([.labels[].name]|index("quality:needs-spec")!=null)}'
```

Observed:

```json
{
  "hasBenchmarks": true,
  "hasNeedsSpec": false,
  "hasReady": true,
  "labels": [
    "type:feature",
    "comp:cortex",
    "comp:inference",
    "prio:high",
    "status:in-progress",
    "size:M",
    "scope:full",
    "quality:ready",
    "type:security"
  ],
  "state": "OPEN"
}
```

Issue Quality Gate:

```text
Issue Quality Gate 26645872696 completed with success.
```

## AC Matrix

| AC | Evidence | Status |
| --- | --- | --- |
| AC-1 | Pending implementation. | Pending |
| AC-2 | Pending implementation. | Pending |
| AC-3 | Pending implementation. | Pending |
| AC-4 | Pending implementation. | Pending |
| AC-5 | Pending implementation. | Pending |

## Not Tested Yet

- Go Gateway tests: pending implementation.
- Deploy-VM runtime: pending; gateway remains inactive unless explicitly approved.
