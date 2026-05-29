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
| AC-2 | Pipeline filters unauthorized extracted `tool_use` actions before response/persistence. Focused proxy test passed. | PASS |
| AC-3 | Rejected action writes `agent_action_rejected` audit event with reason/tool/security metadata into the Event Store. Focused proxy test passed. | PASS |
| AC-4 | Operator-chat injection regression blocks a forbidden `file_write` action and persists only an audit event. Focused proxy test passed. | PASS |
| AC-5 | Legitimate baseline `move` action still appears in the response and persists as `agent_action_received`. Focused proxy test passed. | PASS |

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

## AC-2 / AC-3 Evidence

Command:

```bash
cd cmd/cortex-gateway
go test ./internal/proxy -run TestPipelineRejectsUnauthorizedToolUseAndAudits -v
```

Observed:

```text
=== RUN   TestPipelineRejectsUnauthorizedToolUseAndAudits
2026/05/29 17:30:08 WARN 3-source assembly failed, using fallback agent_id=1 error="assembler not configured, use NewWithAssembler"
2026/05/29 17:30:08 WARN agent action rejected by capability policy request_id=req-injection-001 agent_id=1 agent_name="Thomas Mueller" action_type=tool_use target=file_write:/etc/passwd reason=tool_not_allowed
2026/05/29 17:30:08 INFO pipeline request completed provider=mock request_class=agent_runtime effective_model=test-model policy_source=agent_runtime_policy duration=631.908µs tokens=7 actions=0 agent_id=1 agent_name="Thomas Mueller"
--- PASS: TestPipelineRejectsUnauthorizedToolUseAndAudits (0.00s)
PASS
ok  	github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/proxy	0.011s
```

Package check:

```bash
cd cmd/cortex-gateway
go test ./internal/proxy
```

Observed:

```text
ok  	github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/proxy	0.940s
```

## AC-4 / AC-5 Evidence

Command:

```bash
cd cmd/cortex-gateway
go test ./internal/proxy -run 'TestOperatorChatInjectionCannotPersistForbiddenToolAction|TestPipelineAllowsLegitimateMoveActionWithPolicy' -v
```

Observed:

```text
=== RUN   TestOperatorChatInjectionCannotPersistForbiddenToolAction
2026/05/29 17:32:09 WARN 3-source assembly failed, using fallback agent_id=15 error="assembler not configured, use NewWithAssembler"
2026/05/29 17:32:09 WARN agent action rejected by capability policy request_id=req-operator-injection-001 agent_id=15 agent_name="Hannah Meier" action_type=tool_use target=file_write:payroll.csv reason=tool_not_allowed
2026/05/29 17:32:09 INFO pipeline request completed provider=mock request_class=agent_runtime effective_model=test-model policy_source=agent_runtime_policy duration=792.8µs tokens=9 actions=0 agent_id=15 agent_name="Hannah Meier"
--- PASS: TestOperatorChatInjectionCannotPersistForbiddenToolAction (0.01s)
=== RUN   TestPipelineAllowsLegitimateMoveActionWithPolicy
2026/05/29 17:32:09 WARN 3-source assembly failed, using fallback agent_id=1 error="assembler not configured, use NewWithAssembler"
2026/05/29 17:32:09 INFO pipeline request completed provider=mock request_class=agent_runtime effective_model=test-model policy_source=agent_runtime_policy duration=2.520188ms tokens=6 actions=1 agent_id=1 agent_name="Thomas Mueller"
--- PASS: TestPipelineAllowsLegitimateMoveActionWithPolicy (0.01s)
PASS
ok  	github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/proxy	0.037s
```

Package check:

```bash
cd cmd/cortex-gateway
go test ./internal/proxy ./internal/capability
```

Observed:

```text
ok  	github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/proxy	1.013s
ok  	github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/capability	(cached)
```

## Not Tested Yet

- Go Gateway tests: pending implementation.
- Deploy-VM runtime: not run in this task; gateway remains inactive unless explicitly approved.
