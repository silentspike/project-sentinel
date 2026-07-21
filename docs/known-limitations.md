# Known Limitations — v0.1.0-alpha

This document collects what *does not yet work* in the current release so a
reader can calibrate expectations before they spend an hour cloning and
running things.

The companion document [docs/togaf-gap-v22.md](togaf-gap-v22.md) lists what
*is* implemented per architecture cluster.

---

## What the docker demo does NOT exercise

The `docker-compose.demo.yml` stack is deliberately a behavioral demo:
ECS world, bio-engine, physics, event-sourcing, gateway pipeline, dashboard.

It does **not** exercise the kernel-bound sandbox primitives, because a plain
unprivileged container does not expose user namespaces, cgroups v2,
`CAP_BPF`, a writable bpf-fs, or `/dev/fuse`:

| Component                  | Demo container | VM deploy |
|----------------------------|----------------|-----------|
| bwrap + Landlock isolation | no             | yes       |
| cgroups v2 per-agent caps  | no             | yes       |
| full-cage per-agent netns  | no             | yes       |
| eBPF probes (aya-rs)       | no             | yes       |
| sentinel-fs CAS-FUSE       | no             | yes       |
| Zenoh SHM transport        | no (TCP only)  | yes       |

The `SandboxEnforcer`
(`crates/sentinel-sandbox/src/enforcer.rs`) detects the absence of these
features at boot and degrades gracefully — warnings in the daemon log are
expected demo signal, not failures.

For the full stack with sandbox enforcement, see
`deploy/systemd/*.service` and the deployment notes in
[docs/governance.md](governance.md).

## Demo binaries are Linux x86_64 only

The release ships pre-built binaries for `linux-x86_64` only. Apple Silicon,
arm64, or other architectures need to use the cargo-remote or local-cargo
tier (see `make demo-binaries`).

## CodeQL on the v0.1.0-alpha commit

GitHub Advanced Security (which CodeQL needs to upload SARIF results) is
free on public repositories but a paid feature on private ones. The CodeQL
workflow files ship pre-configured (`build-mode: manual` for Go,
`build-mode: none` for the Bun dashboard, with `paths-ignore` for the
node_modules tree) and will go green on the first scheduled run after the
public flip.

## Tag verification badge

`v0.1.0-alpha` displays "Unverified" in the GitHub web UI until the
maintainer's Ed25519 SSH key is registered as a *Signing Key* on GitHub
(separate from the *Authentication Key* used for `git push`). The tag is
cryptographically valid — verify locally with `git tag -v v0.1.0-alpha`.

## Demo dashboard does not enable the LLM pipeline by default

The demo `cortex-gateway.toml` selects an `ollama` provider on
`http://localhost:11434` so the demo does not require Anthropic credentials.
If you do not have a local ollama serving `qwen3:7b`, agent calls will not
succeed — the dashboard still shows ECS state, room telemetry, and bio
events, which is the intent of a behavioral demo.

To wire a real provider, set the corresponding API key in `.env` and edit
`config/demo/cortex-gateway.toml` to enable `claude-code` or
`anthropic-direct`.

## 60 LLM-persona agents vs 5 in demo

The architecture defines 60 agents in `config/agents/AGENT-*.toml`. The
demo `daemon.toml` caps `max_agents = 5` for the 10-minute window so the
log stays readable and the CPU stays available. To run the full cohort,
remove or raise the cap, expect higher steady-state CPU and memory.

## Gaia generates config, not a zero-downtime live migration

`sentinel-gaia` can generate and validate a complete company config tree
(`gaia-spec.toml`, Agent TOMLs, `rooms.toml`, `daemon.toml`, and
`nightrun.toml`) and can run a daemon dry-run against that tree. It does not
perform a zero-downtime migration of an already running production company.
Operators still need to place the generated config under the intended runtime
path and restart or roll services according to their deployment procedure.

## Nightrun is one-shot in the demo

`sentinel-nightrun` is included in the image but the demo compose stack
does not run it — it is a shift-change batch process designed to fire at
real shift boundaries (06:00 / 14:00 / 22:00). To trigger manually inside
the running stack:

```bash
docker exec sentinel-demo-daemon /usr/local/bin/sentinel-nightrun --config /opt/sentinel/config-runtime/nightrun.toml
```

## Open issues that may matter to a reviewer

Live tracking lives at
<https://github.com/silentspike/project-sentinel/issues> and includes:

- Performance: tick-loop hot-path, dashboard polling, non-blocking nightrun
- Resilience: daemon hardening

These are intentionally open — they reflect post-v0.1.0-alpha roadmap, not
regressions.
