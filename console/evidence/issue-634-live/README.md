# Issue 634 Policy and Patch Registry Evidence

This evidence covers repository policy and CI enforcement only.

```text
runtime_target_class=NONE
deploy_targets=none
read_only_targets=none
benchmark_targets=N/A
runtime_behavior=NOT TESTED
rollback=PR revert
```

No runtime node, service, deployment path, Cargo manifest, or lockfile was changed.
No Rust gate was required because the implementation contains Markdown, Python, and
an always-running CI lint step only.

## AC Mapping

| AC | Result | Evidence |
| --- | --- | --- |
| AC-1 | Four intervention stages, eight ownership decisions, entry/exit criteria, security responsibility, and the LLM non-evidence rule are normative. | `docs/dependency-policy.md` sections 1-4 and 7 |
| AC-2 | The complete safe replacement lifecycle and two explicitly non-authorizing worked examples are present. | `docs/dependency-policy.md` sections 5, 6, and 10 |
| AC-3 | The patch/fork registry has no invented intervention rows; its direct-Git allowlist contains exactly the four real official Aya declarations and is checked bidirectionally. | `docs/dependency-patches.md`, real-tree check below |
| AC-4 | The real tree passes; 15 checker tests cover the four issue-required failures, Git-source drift, `[replace]`, Cargo source replacement, and Rust CI input routing; the checker is in the always-running lint job. | `scripts/check-patch-registry.py`, `scripts/tests/test_check_patch_registry.py`, `.github/workflows/ci.yml`, `negative-tests.md` |
| AC-5 | Policy and registry link #617, #621, #631, #705, and #656; #705 is named as the decision authority. | Cross-link readback below |
| AC-6 | The upgrade playbook governs KEEP, WRAP, PATCH_UPSTREAM, FORK_TEMPORARY, OWN_MINIMAL, and OWN_STRATEGIC without blind auto-merge. | `docs/dependency-policy.md` section 9 |

## Real Cargo Override State

Command:

```bash
python3 scripts/check-patch-registry.py
```

Output:

```text
patch-registry=PASS overrides=0 registry_entries=0 direct_git_dependencies=4
```

The four direct Git declarations point to the official Aya upstream. They match
four machine-readable allowlist rows and are not patch/fork override rows:

```bash
rg -n '^[A-Za-z0-9_-]+\s*=\s*\{[^\n]*\bgit\s*=' \
  --glob 'Cargo.toml' --glob '!target/**'
```

```text
crates/sentinel-ebpf-probes/Cargo.toml:19:aya-ebpf = { git = "https://github.com/aya-rs/aya", default-features = false }
crates/sentinel-ebpf-probes/Cargo.toml:20:aya-log-ebpf = { git = "https://github.com/aya-rs/aya", default-features = false }
crates/sentinel-ebpf/Cargo.toml:26:aya = { git = "https://github.com/aya-rs/aya", default-features = false, optional = true }
crates/sentinel-ebpf/Cargo.toml:27:aya-log = { git = "https://github.com/aya-rs/aya", default-features = false, optional = true }
```

## Checker Tests

```bash
python3 -m unittest discover -s scripts/tests -p 'test*.py'
```

```text
.................................................
----------------------------------------------------------------------
Ran 49 tests

OK
```

The issue-specific verbose run:

```bash
python3 -m unittest -v scripts/tests/test_check_patch_registry.py
```

```text
test_allowlisted_official_git_dependency_passes ... ok
test_empty_registry_matches_tree_without_overrides ... ok
test_expired_temporary_fork_fails ... ok
test_git_dependency_url_change_to_fork_fails ... ok
test_missing_required_field_fails ... ok
test_new_unallowlisted_git_dependency_fails ... ok
test_registered_patch_passes_exact_source_match ... ok
test_registered_replace_passes ... ok
test_registered_source_replacement_passes ... ok
test_rust_ci_filter_covers_every_checker_cargo_input ... ok
test_stale_git_allowlist_row_fails ... ok
test_stale_registry_row_fails ... ok
test_unregistered_patch_fails ... ok
test_unregistered_replace_fails ... ok
test_unregistered_source_replacement_fails ... ok
Ran 15 tests
OK
```

## CI Input Coverage

The `changes` job marks all repository Cargo manifests and both supported
repository Cargo-config names as Rust inputs. Therefore a new or changed monitored
source declaration runs `rust-static`, `rust-doc`, `rust-test`, and `rust-ebpf` in
addition to the unconditional registry checker.

```text
Cargo.toml
**/Cargo.toml
.c[a]rgo/config
.c[a]rgo/config.toml
**/.c[a]rgo/config
**/.c[a]rgo/config.toml
```

## Documentation Gates

```bash
typos docs/dependency-policy.md docs/dependency-patches.md \
  scripts/check-patch-registry.py scripts/tests/test_check_patch_registry.py \
  CHANGELOG.md .github/workflows/ci.yml
```

```text
exit=0
typos=PASS
```

```bash
LC_ALL=C grep -nP '[^\x00-\x7F]' \
  docs/dependency-policy.md docs/dependency-patches.md \
  scripts/check-patch-registry.py scripts/tests/test_check_patch_registry.py
```

```text
exit=1
matches=0
ascii=PASS
```

```bash
python3 scripts/dependency-reachability-audit.py check-public-evidence \
  console/evidence/issue-634-live \
  docs/dependency-policy.md docs/dependency-patches.md
```

```text
public-evidence-scan=PASS files=5
```

## Cross-Link Readback

```bash
gh issue view 634 --json body --jq .body |
  rg -o '#(617|621|631|705|656)' | sort -u
```

```text
#617
#621
#631
#656
#705
```

```bash
gh issue view 656 --json body --jq .body |
  rg -o '#(621|631|634|705)' | sort -u
```

```text
#621
#631
#634
#705
```
