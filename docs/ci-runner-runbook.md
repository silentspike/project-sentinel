# Runbook: CI Self-Hosted Runner (Ephemeral) - Incident Response + Migration

Issue #435. Companion to `deploy/ci-runner/` (the provisioning assets). This runbook
covers the CT-150 incident context, the ephemeral-runner solution, the secret-free token
strategy, and the **Part B live migration** (the step that actually closes #435).

## Scope

How to run, fall back, and migrate the self-hosted GitHub Actions runners for
`silentspike/project-sentinel` from a persistent (incident-prone) setup to a purely
ephemeral one (one job per runner, then self-destruct + re-provision).

## Infrastructure Context

| Item | Value |
|------|-------|
| Runner host (current, persistent) | CT 150, LXC on Proxmox `10.0.0.106`, IP `10.0.0.127` |
| Runner instances (current) | `/opt/actions-runner` + `-2`, `-3`, `-4` (persistent) |
| Organization | `silentspike` |
| Runner group | Default group, `allows_public_repositories: false` (private repos only) |
| Public repos | use GitHub-hosted `ubuntu-latest` (self-hosted runners are private-only) |
| Emergency switch | repo variable `RUNNER_MODE=hosted` (all 16 workflows honor it) |

## Incident 2026-05-30 (Root Cause)

A stuck **CodeQL Go extractor** on a persistent runner drove the Proxmox host to
**load 32**. After the offending process was killed it left **unreapable kernel zombies
(`Zl` / D-state)** that kept the runner cgroup busy, so **CT 150 could not be restarted
without a node reboot**. Separately, CT 150 had accumulated **136 GB of CodeQL cruft**
across jobs. Both failure modes (cgroup remnants, bloat across job boundaries) are
typical of persistent runners and are exactly what ephemeral runners eliminate.

## Ephemeral Solution

Each runner is registered with `config.sh --ephemeral`, executes exactly **one job**,
then deregisters itself and exits. A systemd unit with `Restart=always` re-provisions a
fresh runner (new token, new registration) for the next job. Consequences:

- No process/cgroup state crosses a job boundary -> no zombie-remnant build-up.
- The job workdir (`_work/`, the CodeQL/cargo bloat) is wiped per re-provision ->
  no bloat accumulation.
- The runner **binary** stays cached (`bin/`, `externals/`, `config.sh`, `run.sh`) ->
  no 100 MB re-download and no `api.github.com` call per job (which would hit the
  60/h unauthenticated rate limit).

## Provisioning

See `deploy/ci-runner/README.md` for the asset usage. Summary:

```bash
# Install the assets onto the runner host (Part B):
sudo install -m755 install-ephemeral-runner.sh /opt/actions-runner/install-ephemeral-runner.sh
sudo install -m644 github-runner-ephemeral@.service /etc/systemd/system/
sudo systemctl daemon-reload
# One slot:
sudo systemctl enable --now github-runner-ephemeral@runner-1.service
# Multiple slots:
sudo systemctl enable --now github-runner-ephemeral@runner-2.service
```

The systemd template wipes only `_work/`, `.runner`, `.credentials` per re-provision
(`ExecStartPre`); the cached binary and `HOME`/`CARGO_HOME` stay inside the writable
workdir so `ProtectSystem=strict` does not choke CI jobs (cargo, CodeQL).

## Token Strategy (security-honest)

Only a **short-lived** registration token (~1 h) flows through
`install-ephemeral-runner.sh`. The **long-lived** credential lives only in the host-side
token helper (`RUNNER_TOKEN_CMD`, default `/opt/actions-runner/get-token.sh`), never in
the versioned repo. Two ways to provision that helper:

1. **Recommended: GitHub App + JIT config.** A GitHub App (private key, host-only,
   `0600`) calls `POST /orgs/{org}/actions/runners/generate-jitconfig` to mint a
   short-lived, scoped, single-use token per re-provision. Best practice; no long-lived
   secret sitting on the runner.

2. **Alternative (with a warning): fine-grained PAT via `gh api`.** Simpler to set up,
   but the PAT is long-lived and lives on the host -> a larger blast radius if the host
   is compromised. Use the **minimal** scope (only the runner-administration
   permission required); **do not** use a broad classic `admin:org` PAT.

Never put a long-lived credential (PAT, App private key) in the versioned scripts or the
PR. The whole `deploy/ci-runner/` tree must stay reviewable without secrets.

## Part B - Live Migration (tracked ops, AC-5)

Part A (the assets + this runbook) ships in the PR. **Part B is the live migration on
the runner host and is what actually closes #435.** Until Part B is verified, #435 stays
open (no `status:verified` on Part A alone - "code exists" is not "feature live").

1. On Proxmox `10.0.0.106` / CT 150: provision the token helper
   (`/opt/actions-runner/get-token.sh`, mode `0600`), backed by a GitHub App key
   (preferred) or a fine-grained PAT.
2. Replace the persistent instances (`/opt/actions-runner{-2,-3,-4}`) with the ephemeral
   variant, or provision a fresh ephemeral CT/VM. Keep the org runner registration
   (`silentspike`, Default group) pointed at the ephemeral setup.
3. Enable the ephemeral slots: `systemctl enable --now github-runner-ephemeral@runner-{1..N}`.
4. **Verify (AC-5):**
   - Run at least **5 CI jobs and confirm they complete successfully** (green) - not
     merely "the runner started". The ephemeral hardening (`HOME`/`CARGO_HOME` in the
     workdir, `ProtectSystem=strict`) must not choke real cargo/CodeQL jobs.
   - Per job: a fresh runner registers, runs exactly one job, and disappears.
   - `ps aux | grep -iE 'Zl|D'` shows no zombies; cgroup inspection shows no remnants.
   - `du -sh /opt/actions-runner/<instance>/_work` is fresh per job (no bloat growth).
   - **Cache preserved:** `du -sh /opt/actions-runner/<instance>/{bin,externals}` is
     stable across re-provisions (no re-download per job), and `api.github.com` calls
     are much fewer than jobs (authenticate with `GITHUB_TOKEN` if frequency is high).
   - Measure re-provision time (register -> ready).
5. Decommission CT 150 (zombies + 136 GB bloat) in the next `.106` maintenance window.

## Emergency Fallback

If a runner incident recurs and there is no time to debug, set the repo variable
`RUNNER_MODE=hosted` (Settings -> Variables -> Actions). All 16 workflows then run on
GitHub-hosted `ubuntu-latest` immediately - **no node reboot required** (unlike the
2026-05-30 workaround). Clear the variable to return to self-hosted once the runner host
is healthy.

## Decommission CT 150

Once the ephemeral runners are verified (Part B, AC-5), decommission CT 150 in a
`.106` maintenance window: stop + remove the persistent runner instances, clear the
zombie/cgroup state (may require a final node reboot of `.106` if zombies are still
unreapable), reclaim the disk (136 GB CodeQL cruft), and remove CT 150 from the runner
registration.
