# TOGAF Handoff - Issue #442

Worker PR scope: no TOGAF HTML edits and no `docs/gaia-console-architecture.md` commit. The Gaia Console architecture SSOT remains main-session-owned.

## Facts to reflect in the TOGAF/SSOT update

- Issue #442 adds `services/sentinel-gaia-loop`, a standalone Gaia Console runtime distinct from deterministic `services/sentinel-gaia` and from Voice-of-Gaia simulation input.
- Readiness is token-free: it performs a read-only EventStore scan at startup and on `SENTINEL_GAIA_SCAN_INTERVAL_SECS`, subscribes to NATS `sentinel.events.platform_analysis.>`, writes console artifacts under `/opt/sentinel/data/gaia-console`, and never spawns Claude.
- Explicit operator sessions are the only Claude path: `deep` and `setup-interview` run one `claude -p` turn with `--output-format stream-json`, `--max-budget-usd`, a process timeout, `--strict-mcp-config`, and Bash-only local tools.
- Deep mode exposes `sentinel-ctl` only through Bash. Mutating `sentinel-ctl` operations stay confirm-gated by the CLI.
- Setup interview may also call deterministic `sentinel-gaia` to generate or validate setup artifacts. This is separate from the Gaia Console readiness loop.
- The dashboard backend only nests `/api/gaia/*` routes into the existing authenticated API surface. It does not change TLS/server setup.
- The Solid console adds a `GaiaConsoleView` panel for persisted readiness alerts, session index rows, and raw stream-json session output.
- This PR does not add a second autonomous healing loop and does not alter `llm_analyzer`, the ECS tick loop, or `WorldSnapshot`.

## Main-session update target

Update Cluster 04b / Gaia Console wording in both TOGAF copies, language-separated and fact-verified:

- EN repo copy: `docs/architecture/togaf-architecture-guide.html`
- DE SSOT copy: `/home/jan/togaf-llm-architecture-guide.html`

The update should describe Gaia Console readiness as token-free/subscription-and-scan based, and explicit Deep/Setup sessions as budget/timeout-capped Claude Code invocations initiated by an operator.
