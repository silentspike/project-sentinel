# Issue #396 Verification

Issue: https://github.com/silentspike/project-sentinel/issues/396

## Scope

Docs-only runtime-contract decision for TOGAF v22.2 Cluster 12. The user has
explicitly approved the decision: WASM/WASI/Wasmtime is the default
Nano-Container runtime contract; arbitrary native code is available only via an
explicit native Escape-Hatch-Pool. The decision must be recorded in the
Deviation Register, not in a public `docs/adr/` document.

## Issue Readiness Evidence

Command:

```bash
gh issue view 396 --json labels,body | jq '{labels:[.labels[].name], hasBenchmarks:(.body|contains("## Benchmarks"))}'
```

Observed:

```json
{
  "labels": [
    "type:docs",
    "comp:sandbox",
    "prio:high",
    "status:in-progress",
    "size:M",
    "scope:full",
    "quality:ready"
  ],
  "hasBenchmarks": true
}
```

Issue Quality Gate:

```text
Issue Quality Gate 26643915941 completed with success.
```

## AC Matrix

| AC | Evidence | Status |
| --- | --- | --- |
| AC-1 | DEV-006 documents both WASM/WASI and arbitrary native-code options with density/freedom/portability/security trade-offs. | PASS |
| AC-2 | DEV-006 records the user-approved decision: WASM/WASI on Wasmtime is the default runtime; native code is an explicit Escape-Hatch-Pool. | PASS |
| AC-3 | DEV-006 records the runtime contract, default runtime, native exception path, and Cluster 12 consequences. | PASS |
| AC-4 | Tracking epic #397 has a comment linking #396 to DEV-006 and the selected contract. | PASS |

## Benchmarks

Docs-only contract benchmark. Target: 100% of runtime-decision contract elements
validated by script/inline check. Runtime performance is not applicable.

Command:

```bash
python3 - <<'PY'
import json
import subprocess
from pathlib import Path
text = Path('docs/togaf-deviations-v22.md').read_text()
gap = Path('docs/togaf-gap-v22.md').read_text()
comments = subprocess.check_output(['gh', 'issue', 'view', '397', '--json', 'comments'], text=True)
comment_text = '\n'.join(c['body'] for c in json.loads(comments)['comments'])
checks = {
    'dev_006_exists': '## DEV-006' in text,
    'wasm_option': 'WASM/WASI' in text and 'Wasmtime' in text,
    'native_option': 'native' in text and 'Escape-Hatch-Pool' in text,
    'density_tradeoff': 'density' in text,
    'freedom_tradeoff': 'freedom' in text,
    'portability_tradeoff': 'portability' in text,
    'security_tradeoff': 'security' in text,
    'decision_default': 'default Nano-Container runtime contract' in text,
    'runtime_contract': 'Default runtime: `wasm+wasi`' in text,
    'consequences': 'Follow-up work under #397' in text and 'Clusters 00-11 remain untouched' in text,
    'dev_004_intact': 'ADRs live in the internal workspace, not the public repository' in text,
    'cluster_12_gap': 'Cluster 12' in gap and 'Runtime contract decision (#396)' in gap,
    'epic_link_comment': '#396 decision link' in comment_text and 'DEV-006' in comment_text and 'Escape-Hatch-Pool' in comment_text,
    'no_public_adr': not Path('docs/adr').exists(),
}
missing = [name for name, ok in checks.items() if not ok]
for name, ok in checks.items():
    print(f'{name}: {"PASS" if ok else "FAIL"}')
if missing:
    raise SystemExit(f'Missing checks: {missing}')
print('issue_396_contract_check: PASS')
PY
```

Observed:

```text
dev_006_exists: PASS
wasm_option: PASS
native_option: PASS
density_tradeoff: PASS
freedom_tradeoff: PASS
portability_tradeoff: PASS
security_tradeoff: PASS
decision_default: PASS
runtime_contract: PASS
consequences: PASS
dev_004_intact: PASS
cluster_12_gap: PASS
epic_link_comment: PASS
no_public_adr: PASS
issue_396_contract_check: PASS
```

## Non-Applicable Runtime Checks

- Deploy: not applicable for docs-only issue.
- Gateway start: not applicable and intentionally avoided.

## Final Docs Check

Command:

```bash
python3 <combined #390/#396 contract check>
git diff --check -- CHANGELOG.md docs/security/threat-model.md docs/togaf-deviations-v22.md docs/togaf-gap-v22.md test-390-verification.md test-396-verification.md
```

Observed:

```text
390_doc_exists: PASS
390_three_classes: PASS
390_sections_per_class: PASS
390_assets: PASS
390_followups: PASS
390_gap_doc: PASS
396_dev006: PASS
396_options: PASS
396_tradeoffs: PASS
396_default: PASS
396_dev004_intact: PASS
396_cluster12_gap: PASS
396_epic_link: PASS
no_public_adr: PASS
changelog_390_396: PASS
combined_contract_check: PASS
git diff --check: PASS
```
