# TOGAF Handoff - Issue #442

Worker PR scope: no TOGAF HTML edits and no `docs/gaia-console-architecture.md` commit. The Gaia Console architecture SSOT remains main-session-owned.

## Facts to reflect in the TOGAF/SSOT update

- Issue #442 adds `services/sentinel-gaia-loop`, a standalone Gaia Console runtime distinct from deterministic `services/sentinel-gaia` and from Voice-of-Gaia simulation input.
- Readiness is token-free: it performs a read-only EventStore scan at startup and on `SENTINEL_GAIA_SCAN_INTERVAL_SECS`, subscribes to NATS `sentinel.events.platform_analysis.>`, writes console artifacts under `/opt/sentinel/data/gaia-console`, and never spawns Claude.
- Explicit operator sessions are the only Claude path: `deep` and `setup-interview` run one native Claude Code turn with `claude -p`, `--safe-mode`, `--output-format stream-json`, `--max-budget-usd`, a process timeout, `--strict-mcp-config`, `--permission-mode dontAsk`, closed stdin, a minimal child-environment allowlist, and Bash-only local tools.
- Admission is server-enforced: one active session across processes, a distinct-request rate limit, an idempotency key/fingerprint journal, and a rolling USD budget window. Busy and exhausted limits return HTTP 429; reused keys with changed inputs return 409.
- Resume input is a local `gaia-*` session ID only. The backend resolves the corresponding Claude session ID from a successful same-mode Gaia journal entry and never forwards an operator-supplied Claude ID.
- Claude and its tool subprocesses run in a dedicated process group that is killed on timeout and normal completion. The systemd services also carry cgroup memory, task, CPU, and stop-time limits.
- Gaia directories/files are 0700/0600. Dashboard auth is a required persistent root-owned 0600 environment file, and the native Claude 2.1.214 executable is installed root-owned from a repository-pinned SHA-256.
- Deep mode exposes `sentinel-ctl` only through Bash. Mutating `sentinel-ctl` operations stay confirm-gated by the CLI.
- The neutral system prompt injects bounded dynamic company knowledge from generated `company-context.md` as reference data, never as instructions.
- Setup interview may also call deterministic `sentinel-gaia` to generate or validate setup artifacts. Its prompt carries the exact `GaiaSpec` JSON shape and enum spellings; a complete checklist leads to one `sentinel-gaia init --spec-json` call and an isolated daemon dry-run. This is separate from the Gaia Console readiness loop.
- The dashboard backend only nests `/api/gaia/*` routes into the existing authenticated API surface. It does not change TLS/server setup.
- The Solid console adds a `GaiaConsoleView` panel for persisted readiness alerts, session index rows, and raw stream-json session output.
- This PR does not add a second autonomous healing loop and does not alter `llm_analyzer`, the ECS tick loop, or `WorldSnapshot`.

## Main-session update target

Update Cluster 04b / Gaia Console wording in both TOGAF copies, language-separated and fact-verified:

- EN repo copy: `docs/architecture/togaf-architecture-guide.html`
- DE SSOT copy: `/home/jan/togaf-llm-architecture-guide.html`

The update should describe Gaia Console readiness as token-free/subscription-and-scan based, and explicit Deep/Setup sessions as safe-mode, admission-controlled, budget/timeout-capped native Claude Code invocations initiated by an authenticated operator. Live accepted-session costs were USD 0.0206505 for the two-turn Deep/resume flow, USD 0.0197928 for complete Setup generation, and USD 0.004407 for the dashboard stream proof.
