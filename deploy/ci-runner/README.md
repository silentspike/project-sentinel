# CI Runner -- Ephemeral GitHub Actions Runner (Part A, #435)

Versioned assets that provision a **purely ephemeral** self-hosted GitHub Actions runner:
each runner executes exactly **one job**, then self-destructs and is re-provisioned with a
fresh registration. This eliminates the two failure modes of the persistent runner
**CT 150** (Incident 2026-05-30: unreapable kernel zombies from a stuck CodeQL extractor
kept the runner cgroup busy -> node-reboot required, plus 136G of accumulated CodeQL cruft).

> Part A (this directory + the runbook) is repo-feasible and ships in the PR.
> The **live migration** of CT 150 to ephemeral runners is Part B (manual ops on
> Proxmox 10.0.0.106) and is tracked step-by-step in `docs/ci-runner-runbook.md`.
> Issue #435 stays open until Part B is verified.

## Files

| File | Purpose |
|------|---------|
| `install-ephemeral-runner.sh` | Downloads (cache-persistent) + tag-exact SHA-verifies the runner, registers it with `config.sh --ephemeral`, runs one job. |
| `github-runner-ephemeral@.service` | systemd template; `Restart=always` re-provisions after each job (fresh token). `ExecStartPre` wipes only job bloat + stale registration -- the cached binary stays. |
| `README.md` | This file. |

## Usage

```bash
# Ad-hoc / one-shot (token passed inline, ~1h lifetime):
sudo RUNNER_REGISTRATION_TOKEN=<short-lived-token> \
     ./install-ephemeral-runner.sh --url https://github.com/silentspike runner-1

# Dry-run (prints the exact download + SHA-fetch + config.sh commands, executes nothing):
RUNNER_REGISTRATION_TOKEN=dummy \
  ./install-ephemeral-runner.sh --dry-run --url https://github.com/silentspike runner-1

# Persistent slot via systemd (Part B target):
sudo install -m755 install-ephemeral-runner.sh /opt/actions-runner/install-ephemeral-runner.sh
sudo install -m644 github-runner-ephemeral@.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now github-runner-ephemeral@runner-1.service
# (multiple slots: ...@runner-2.service, ...@runner-3.service, ...)
```

## Token interface (secret-free, ORC decision)

Only a **short-lived** registration token (~1h) flows through `install-ephemeral-runner.sh`.
The **long-lived** credential lives only in the host-side token helper, never in this
versioned directory (the PR is fully reviewable without secrets).

- `RUNNER_REGISTRATION_TOKEN` -- short-lived token for ad-hoc provisioning.
- `RUNNER_TOKEN_CMD` -- host-side helper (default `/opt/actions-runner/get-token.sh`) that
  returns a fresh token. Part B provisions this helper with a GitHub-App key (preferred)
  or a fine-grained PAT. See `docs/ci-runner-runbook.md` -> *Token Strategy*.
- `GITHUB_TOKEN` (optional) -- authenticates the `api.github.com` SHA-256 fetch
  (5000/h instead of 60/h unauthenticated; avoids a rate-limit wall at high job frequency).

## Environment knobs

| Variable | Default | Notes |
|----------|---------|-------|
| `RUNNER_VERSION` | `2.329.0` | Must be >= 2.329.0 (GitHub blocks older runners). |
| `RUNNER_LABELS` | `self-hosted,linux,x64` | Must match the workflow `runs-on` labels. |
| `RUNNER_TOKEN_CMD` | `/opt/actions-runner/get-token.sh` | Part B token helper. |
| `RUNNER_REGISTRATION_TOKEN` | *(unset)* | Ad-hoc override of the helper. |
| `GITHUB_TOKEN` | *(unset)* | Optional API auth for the SHA fetch. |

## See also

- `docs/ci-runner-runbook.md` -- incident context, token strategy, **Part B live-migration
  steps + verify criteria**, emergency fallback (`RUNNER_MODE=hosted`), CT 150 decommission.
- `.github/workflows/*.yml` -- every workflow already carries the `RUNNER_MODE` switch
  (`runs-on: ${{ vars.RUNNER_MODE == 'hosted' && 'ubuntu-latest' || fromJSON('["self-hosted", "linux", "x64"]') }}`),
  so `RUNNER_MODE=hosted` is a working emergency fallback for the whole CI matrix.
