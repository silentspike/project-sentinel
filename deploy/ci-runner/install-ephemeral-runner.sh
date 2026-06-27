#!/usr/bin/env bash
# install-ephemeral-runner.sh - provision a single-job (ephemeral) GitHub Actions runner.
#
# Each invocation: ensure the runner binary is cached -> fetch the tag-exact SHA-256 ->
# ./config.sh --ephemeral -> ./run.sh. After exactly ONE job the runner self-destructs
# (deregisters); a systemd unit with Restart=always then re-provisions a fresh runner
# (new token, new registration). This is the #435 fix for the CT-150 incident
# (2026-05-30: stuck CodeQL extractor -> unreapable kernel zombies -> cgroup blocked ->
# node-reboot required + 136G CodeQL cruft bloat on a persistent runner).
#
# Cache policy (ORC review point 2): the runner BINARY stays cached across re-provisions
# (no 100MB re-download + no api.github.com call per job = no rate-limit wall). Only the
# job workdir (_work/) and stale registration (.runner/.credentials) are wiped per
# re-provision - see the systemd ExecStartPre.
#
# Secret policy (ORC review point 1 / Token-Option 1): only a SHORT-LIVED registration
# token (~1h) flows through this script. The long-lived credential (PAT / GitHub-App key)
# lives ONLY in the Part-B token helper on the host (RUNNER_TOKEN_CMD), never in this
# versioned file -> the PR stays fully secret-free and reviewable.
#
# Usage: install-ephemeral-runner.sh [--dry-run] --url <repo_or_org_url> <instance-name>
# Env:
#   RUNNER_VERSION            runner version, >= 2.329.0 (default 2.329.0; GitHub blocks
#                            older versions - see docs/ci-runner-runbook.md).
#   RUNNER_LABELS             comma-separated labels (default "self-hosted,linux,x64").
#   RUNNER_REGISTRATION_TOKEN short-lived token (~1h) for ad-hoc provisioning.
#   RUNNER_TOKEN_CMD          host-side token helper (default /opt/actions-runner/get-token.sh);
#                            Part B provisions this with a GitHub-App key or PAT.
#   GITHUB_TOKEN              optional; raises api.github.com rate limit 60/h -> 5000/h.
set -euo pipefail

RUNNER_VERSION="${RUNNER_VERSION:-2.329.0}"
RUNNER_LABELS="${RUNNER_LABELS:-self-hosted,linux,x64}"
RUNNER_TOKEN_CMD="${RUNNER_TOKEN_CMD:-/opt/actions-runner/get-token.sh}"
URL=""
INSTANCE=""
DRY_RUN=0

usage() {
  cat <<EOF
Usage: $0 [--dry-run] --url <repo_or_org_url> <instance-name>

Provisions an ephemeral GitHub Actions runner (1 job -> self-destruct).
Env: RUNNER_VERSION (>=2.329.0), RUNNER_LABELS, RUNNER_REGISTRATION_TOKEN | RUNNER_TOKEN_CMD,
     GITHUB_TOKEN (optional, raises api rate limit).
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --url) URL="${2:-}"; shift 2 ;;
    --dry-run) DRY_RUN=1; shift ;;
    -h|--help) usage; exit 0 ;;
    --*) echo "error: unknown option: $1" >&2; usage >&2; exit 2 ;;
    *)
      if [[ -z "$INSTANCE" ]]; then INSTANCE="$1"
      else echo "error: unexpected extra argument: $1" >&2; exit 2; fi
      shift ;;
  esac
done

if [[ -z "$URL" ]]; then echo "error: --url is required" >&2; usage >&2; exit 2; fi
if [[ -z "$INSTANCE" ]]; then echo "error: instance name is required" >&2; usage >&2; exit 2; fi
# Dry-run only prints commands (no fs/network); real provisioning needs root.
if [[ "$DRY_RUN" -eq 0 && "$(id -u)" -ne 0 ]]; then
  echo "error: run as root (manages /opt/actions-runner ownership); --dry-run skips this" >&2
  exit 1
fi

WORKDIR="/opt/actions-runner/${INSTANCE}"
TARBALL="actions-runner-linux-x64-${RUNNER_VERSION}.tar.gz"

log()  { echo "[install-ephemeral-runner ${INSTANCE}] $*"; }
warn() { echo "[install-ephemeral-runner ${INSTANCE}] WARNING: $*" >&2; }
# In dry-run, print the command instead of executing it.
run() {
  if [[ "$DRY_RUN" -eq 1 ]]; then
    printf '  [dry-run] %s\n' "$*"
  else
    "$@"
  fi
}

log "ephemeral runner provisioning (version=${RUNNER_VERSION}, labels=${RUNNER_LABELS}, dry_run=${DRY_RUN})"

# --- workdir + binary cache (persistent across re-provisions) ---
if [[ "$DRY_RUN" -eq 0 ]]; then mkdir -p "$WORKDIR"; fi

if [[ -x "${WORKDIR}/run.sh" ]]; then
  log "runner binary cached -> skip download (no api.github.com call this cycle)"
else
  log "runner binary not cached -> downloading ${TARBALL}"
  run curl -fL --retry 3 -o "${WORKDIR}/${TARBALL}" \
    "https://github.com/actions/runner/releases/download/v${RUNNER_VERSION}/${TARBALL}"

  # Tag-exact SHA-256 from the GitHub API (NOT releases/latest - that would mismatch as
  # soon as GitHub ships a newer version; NOT hardcoded - that would rot). The release
  # asset carries .digest as "sha256:<hex>".
  auth_args=()
  if [[ -n "${GITHUB_TOKEN:-}" ]]; then auth_args=(-H "Authorization: Bearer ${GITHUB_TOKEN}"); fi
  api_url="https://api.github.com/repos/actions/runner/releases/tags/v${RUNNER_VERSION}"
  if [[ "$DRY_RUN" -eq 1 ]]; then
    EXPECTED_SHA="<fetched-at-runtime from ${api_url} asset .digest>"
    printf '  [dry-run] curl -fsSL %s %s | jq -r %s\n' \
      "${auth_args[*]:-}" "$api_url" "'.assets[] | select(.name==\"$TARBALL\") | .digest'"
  else
    EXPECTED_SHA="$(curl -fsSL "${auth_args[@]}" "$api_url" \
      | jq -r --arg asset "$TARBALL" '.assets[] | select(.name==$asset) | .digest // empty' \
      | sed 's/^sha256://')"
    if [[ -z "$EXPECTED_SHA" ]]; then
      echo "error: could not fetch tag-exact SHA for ${TARBALL} (v${RUNNER_VERSION}) from ${api_url}" >&2
      exit 1
    fi
  fi
  log "verifying SHA-256 (tag-exact): ${EXPECTED_SHA}"
  if [[ "$DRY_RUN" -eq 0 ]]; then
    ACTUAL_SHA="$(sha256sum "${WORKDIR}/${TARBALL}" | awk '{print $1}')"
    if [[ "$ACTUAL_SHA" != "$EXPECTED_SHA" ]]; then
      echo "error: SHA-256 mismatch for ${TARBALL} (expected ${EXPECTED_SHA}, got ${ACTUAL_SHA})" >&2
      exit 1
    fi
    log "SHA-256 verified"
  fi

  log "extracting ${TARBALL}"
  run tar -xzf "${WORKDIR}/${TARBALL}" -C "$WORKDIR"
  run rm -f "${WORKDIR}/${TARBALL}"
fi

# --- short-lived registration token via interface (never hardcoded) ---
if [[ -n "${RUNNER_REGISTRATION_TOKEN:-}" ]]; then
  TOKEN="$RUNNER_REGISTRATION_TOKEN"
  log "token source: RUNNER_REGISTRATION_TOKEN (ad-hoc, ~1h)"
elif [[ -x "$RUNNER_TOKEN_CMD" ]]; then
  TOKEN="$("$RUNNER_TOKEN_CMD")"
  log "token source: RUNNER_TOKEN_CMD ($RUNNER_TOKEN_CMD)"
else
  echo "error: no registration token. Set RUNNER_REGISTRATION_TOKEN (~1h, ad-hoc) or" >&2
  echo "       provide an executable RUNNER_TOKEN_CMD (default ${RUNNER_TOKEN_CMD})." >&2
  echo "       See docs/ci-runner-runbook.md (Token Strategy) for the Part-B helper." >&2
  exit 1
fi
if [[ -z "$TOKEN" ]]; then
  echo "error: token helper returned an empty token" >&2
  exit 1
fi

# --- ephemeral registration: the #435 core (1 job -> self-destruct) ---
# Token is passed to config.sh (not echoed); --unattended avoids the interactive prompt.
TOKEN_ARG="$TOKEN"
if [[ "$DRY_RUN" -eq 1 ]]; then TOKEN_ARG="***REDACTED***"; fi
log "config.sh --ephemeral (registers for exactly 1 job, then self-destructs)"
run "${WORKDIR}/config.sh" --ephemeral \
  --url "$URL" --token "$TOKEN_ARG" \
  --labels "$RUNNER_LABELS" --name "$INSTANCE" --unattended

# --- run: blocks until exactly one job ends; --ephemeral -> runner exits afterwards ---
log "run.sh (blocks until the single job ends)"
run "${WORKDIR}/run.sh"

log "job ended -> runner self-destructed; systemd Restart=always will re-provision (fresh token)"
