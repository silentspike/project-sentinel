# Private Repo Guardrails

This repository is intentionally private. We still enforce a strict delivery process so that agent output is verifiable and regressions are caught early.

## Why this exists

Private repos without GitHub Pro branch protection cannot hard-enforce required status checks at merge time.
This document defines compensating controls.

## Active controls

1. **Issue templates with contract fields**
   - `.github/ISSUE_TEMPLATE/bug_report.yml`
   - `.github/ISSUE_TEMPLATE/feature_request.yml`
   - Required fields: scope, acceptance criteria (`AC-*`), verify, evidence plan.

2. **Auto labeling**
   - `.github/workflows/auto-label.yml`
   - Applies `type:*`, `status:*`, `size:*`, `scope:*`, and `quality:needs-spec` defaults.

3. **Issue quality gate**
   - `.github/workflows/issue-quality.yml`
   - Validates issue structure and promotes from `quality:needs-spec` to `quality:ready`.

4. **PR quality gate**
   - `.github/workflows/pr-quality.yml`
   - Requires linked issue keywords (e.g. `Closes #123`), quality-ready issues, and AC evidence mapping in PR body.

5. **Main push guard**
   - `.github/workflows/main-push-guard.yml`
   - Detects pushes to `main` without PR association and raises an incident issue.

6. **Build and security CI**
   - `.github/workflows/ci.yml`
   - `.github/workflows/security.yml`
   - `.github/workflows/deny.yml`
   - `.github/workflows/codeql.yml`

## Local controls

1. Install hooks:
   - `make hooks`
   - Sets `core.hooksPath=.githooks`

2. Pre-push policy:
   - `.githooks/pre-push`
   - Blocks direct push to `main` (unless `ALLOW_MAIN_PUSH=1`)
   - Runs `make ci` by default

3. Safe merge helper:
   - `make safe-merge PR=<number> [METHOD=merge|squash|rebase]`
   - Verifies:
     - PR is open and not draft
     - linked issues are `quality:ready`
     - required workflows passed (`CI`, `PR Lint`, `PR Quality Gate`)

## Required operator flow

1. Create issue via template.
2. Wait until issue quality gate passes (`quality:ready`).
3. Implement on feature branch.
4. Open PR with linked issue and AC evidence table.
5. Wait for CI + PR gates.
6. Merge via `make safe-merge PR=<n>`.

## Incident handling

If `Main Push Guard` fails:
1. Inspect generated incident issue.
2. Confirm whether commit was intentional emergency change.
3. Backfill missing PR/audit trail immediately.
