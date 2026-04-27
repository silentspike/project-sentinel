# Examples — copy-pasteable runtime-governance walkthroughs

Three short walkthroughs, each self-contained and runnable against the
demo stack (`make demo`) or a VM deploy.

| File | Use case | Runs against |
|------|----------|--------------|
| [`minimal-sandbox-policy.toml`](minimal-sandbox-policy.toml) | Per-agent sandbox config — bwrap + Landlock + cgroups + netns defaults | VM deploy |
| [`audit-replay-pattern.md`](audit-replay-pattern.md) | Save snapshot, restart daemon, verify deterministic replay | demo or VM |
| [`control-plane-pattern.md`](control-plane-pattern.md) | Read three control-plane decision ledgers, verify boundary isolation | demo or VM |

Each walkthrough lists its pre-conditions, the commands to run, and the
expected output. They are tool-agnostic — no Codex/Claude/Gemini-specific
assumptions — and each is under 100 lines so they stay easy to read.

For a longer guided session see
[`docs/workshop-agent-runtime-governance.md`](../docs/workshop-agent-runtime-governance.md).
