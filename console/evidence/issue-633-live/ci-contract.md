# Required CI Contract

Maintainer decision:

- Cargo Bans is required through the CI DAG.
- `ci-pass` remains the only direct branch-protection context.
- The path-filtered `Supply Chain / Bans` context is not separately required.

Live branch-protection readback before implementation:

```bash
gh api repos/silentspike/project-sentinel/branches/main/protection/required_status_checks \
  --jq '{strict,contexts}'
```

```json
{"contexts":["ci-pass"],"strict":true}
```

Workflow structure check:

```text
workflow_yaml=PASS
bans_job=conditional
ci_pass_needs_bans=true
cargo_policy_paths=9/9
```

The conditional Bans job is triggered by root or nested `Cargo.toml`,
`Cargo.lock`, `deny.toml`, and repository-controlled Cargo config files. Its
result is evaluated by the always-running `ci-pass` aggregation job.
