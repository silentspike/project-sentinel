# Issue #693 Contract-Gate Evidence

Date: 2026-07-20
Base: `dade246e244bf1809200da5c0464e80bc79c5cdf`
Scope: repository contract, profile, matrix, validator, tests, and CI integration

No runtime service or VM was contacted or changed. Runtime requirements owned by #75, #472, #650, and #694-#696 remain explicitly blocked or unverified in the matrix.

## Positive Validation

Command:

```bash
python3 scripts/product-acceptance/check_contract.py --check
```

Output:

```text
M0 contract validation passed: scripts/product-acceptance/m0-contract.toml
```

## Negative Validation

Command:

```bash
python3 -m unittest discover -s scripts/product-acceptance -p 'test_*.py' -v
```

Result:

```text
Ran 18 tests in 0.385s

OK
```

The tests reject duplicate requirement IDs, duplicate gate lists, category/ID mismatches, missing fields, missing categories, unknown owners, unknown statuses, missing pass evidence, blocked rows without reasons, missing contract headings, incomplete roles, unknown quality runners, cluster-required M0 profiles, and evidence paths or symlinks outside the repository.

## TOML Parsing

Command:

```bash
python3 - <<'PY'
from pathlib import Path
import tomllib
for path in (
    Path('config/work-profiles/web-project-v1.toml'),
    Path('scripts/product-acceptance/m0-contract.toml'),
):
    with path.open('rb') as handle:
        tomllib.load(handle)
    print(f'PASS {path}')
PY
```

Output:

```text
PASS config/work-profiles/web-project-v1.toml
PASS scripts/product-acceptance/m0-contract.toml
```

## CI Contract

The always-on `lint` job executes both the positive validator and all negative tests. The gate does not require Rust and does not run runtime benchmarks on the Rust build server.
