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
| AC-1 | `cmd/cortex-gateway/internal/capability/agent_policy.go` loads per-agent `[capabilities].tools` from agent TOML and validates tool/target permissions. Focused Go tests and real config count passed. | PASS |
| AC-2 | Pending implementation. | Pending |
| AC-3 | Pending implementation. | Pending |
| AC-4 | Pending implementation. | Pending |
| AC-5 | Pending implementation. | Pending |

## AC-1 Evidence

Command:

```bash
cd cmd/cortex-gateway
go test ./internal/capability -run 'TestLoadAgentActionPolicyFromAgentTOML|TestAgentActionPolicy' -v
```

Observed:

```text
=== RUN   TestLoadAgentActionPolicyFromAgentTOML
--- PASS: TestLoadAgentActionPolicyFromAgentTOML (0.00s)
=== RUN   TestAgentActionPolicyAllowsConfiguredTool
--- PASS: TestAgentActionPolicyAllowsConfiguredTool (0.00s)
=== RUN   TestAgentActionPolicyRejectsUnconfiguredTool
--- PASS: TestAgentActionPolicyRejectsUnconfiguredTool (0.00s)
=== RUN   TestAgentActionPolicyRejectsUnconfiguredTarget
--- PASS: TestAgentActionPolicyRejectsUnconfiguredTarget (0.00s)
=== RUN   TestAgentActionPolicyAllowsBaselineNonToolActions
--- PASS: TestAgentActionPolicyAllowsBaselineNonToolActions (0.00s)
PASS
ok  	github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/capability	0.004s
```

Command:

```bash
python3 - <<'PY'
from pathlib import Path
files = sorted(Path('config/agents').glob('AGENT-*.toml'))
missing = [str(p) for p in files if '[capabilities]' not in p.read_text() or 'tools =' not in p.read_text()]
print(f'agent_files={len(files)}')
print(f'capability_definitions={len(files)-len(missing)}')
print(f'missing={len(missing)}')
if missing:
    raise SystemExit('\n'.join(missing[:10]))
print('real_agent_capability_config: PASS')
PY
```

Observed:

```text
agent_files=60
capability_definitions=60
missing=0
real_agent_capability_config: PASS
```

## Not Tested Yet

- Go Gateway tests: pending implementation.
- Deploy-VM runtime: pending; gateway remains inactive unless explicitly approved.
