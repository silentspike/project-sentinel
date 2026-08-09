# Renovate Compatibility

The repository's `renovate.json` has one Cargo rule matching every Rust minor or patch
update and assigning it to the `rust-minor` group. Both aligned benchmark manifests now
refer to the same root workspace Criterion declaration, so there is no independent
crate-local Criterion version for Renovate to split on a later update.

Relevant configuration:

```json
{
  "matchManagers": ["cargo"],
  "matchUpdateTypes": ["minor", "patch"],
  "groupName": "rust-minor"
}
```

Major Rust updates remain separate by policy, but the root workspace declaration is
still the single version owner. No Renovate grouping change is required for DEP-011.

The pre-existing Renovate workflow defect is a separate finding and remains outside
Issues #631 and #632. This PR does not modify `renovate.json` or the workflow.
