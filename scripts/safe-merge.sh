#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 1 ]]; then
  echo "Usage: $0 <pr-number> [merge|squash|rebase]"
  exit 1
fi

PR_NUMBER="$1"
MERGE_METHOD="${2:-merge}"

case "$MERGE_METHOD" in
  merge|squash|rebase) ;;
  *)
    echo "Invalid merge method: $MERGE_METHOD (allowed: merge|squash|rebase)"
    exit 1
    ;;
esac

if ! command -v gh >/dev/null 2>&1; then
  echo "ERROR: gh CLI not found"
  exit 1
fi

if ! command -v jq >/dev/null 2>&1; then
  echo "ERROR: jq not found"
  exit 1
fi

gh auth status >/dev/null 2>&1 || {
  echo "ERROR: gh auth is not configured"
  exit 1
}

REPO="$(gh repo view --json nameWithOwner -q .nameWithOwner)"

PR_JSON="$(gh pr view "$PR_NUMBER" -R "$REPO" --json number,state,isDraft,headRefOid,body,title,url)"
PR_STATE="$(printf '%s' "$PR_JSON" | jq -r '.state')"
PR_DRAFT="$(printf '%s' "$PR_JSON" | jq -r '.isDraft')"
PR_SHA="$(printf '%s' "$PR_JSON" | jq -r '.headRefOid')"
PR_URL="$(printf '%s' "$PR_JSON" | jq -r '.url')"
PR_BODY="$(printf '%s' "$PR_JSON" | jq -r '.body // ""')"

if [[ "$PR_STATE" != "OPEN" ]]; then
  echo "ERROR: PR #$PR_NUMBER is not open"
  exit 1
fi

if [[ "$PR_DRAFT" == "true" ]]; then
  echo "ERROR: PR #$PR_NUMBER is draft"
  exit 1
fi

mapfile -t LINKED_ISSUES < <(
  printf '%s\n' "$PR_BODY" \
    | grep -Eio '(closes|close|fixes|fix|resolves|resolve|addresses|partially addresses)[[:space:]]+#[0-9]+' \
    | grep -Eo '#[0-9]+' \
    | tr -d '#' \
    | sort -u
)

if [[ ${#LINKED_ISSUES[@]} -eq 0 ]]; then
  echo "ERROR: No linked issues found in PR body."
  echo "Expected keywords like: Closes #123"
  exit 1
fi

for ISSUE in "${LINKED_ISSUES[@]}"; do
  ISSUE_JSON="$(gh issue view "$ISSUE" -R "$REPO" --json number,state,labels,title)"
  HAS_READY="$(printf '%s' "$ISSUE_JSON" | jq '[.labels[].name] | index("quality:ready") != null')"
  HAS_NEEDS_SPEC="$(printf '%s' "$ISSUE_JSON" | jq '[.labels[].name] | index("quality:needs-spec") != null')"

  if [[ "$HAS_READY" != "true" ]]; then
    echo "ERROR: Issue #$ISSUE is not quality:ready"
    exit 1
  fi

  if [[ "$HAS_NEEDS_SPEC" == "true" ]]; then
    echo "ERROR: Issue #$ISSUE still has quality:needs-spec"
    exit 1
  fi
done

RUNS_JSON="$(gh run list -R "$REPO" --commit "$PR_SHA" --json name,status,conclusion,event --limit 100)"

required_workflows=(
  "CI"
  "PR Lint"
  "PR Quality Gate"
)

for WORKFLOW in "${required_workflows[@]}"; do
  COUNT="$(printf '%s' "$RUNS_JSON" | jq --arg n "$WORKFLOW" '[.[] | select(.name == $n and .event == "pull_request")] | length')"
  if [[ "$COUNT" -eq 0 ]]; then
    echo "ERROR: Required workflow '$WORKFLOW' has no run for this PR head commit."
    exit 1
  fi

  BAD_COUNT="$(printf '%s' "$RUNS_JSON" | jq --arg n "$WORKFLOW" '[.[] | select(.name == $n and .event == "pull_request" and .conclusion != "success")] | length')"
  if [[ "$BAD_COUNT" -gt 0 ]]; then
    echo "ERROR: Required workflow '$WORKFLOW' is not successful."
    exit 1
  fi
done

echo "All gates passed for $PR_URL"
echo "Merging PR #$PR_NUMBER with method: $MERGE_METHOD"

case "$MERGE_METHOD" in
  merge)
    gh pr merge "$PR_NUMBER" -R "$REPO" --merge --delete-branch
    ;;
  squash)
    gh pr merge "$PR_NUMBER" -R "$REPO" --squash --delete-branch
    ;;
  rebase)
    gh pr merge "$PR_NUMBER" -R "$REPO" --rebase --delete-branch
    ;;
esac
