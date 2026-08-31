# AC-11 real provider and cost lineage

Result: PASS.

One explicitly bounded request exercised the ChatGPT-backed Codex CLI path.

- Provider/model: `codex-cli` / `gpt-5.6-luna`.
- Pinned CLI: `codex-cli 0.151.0`.
- Authentication readiness: `Logged in using ChatGPT`.
- Request ID: `issue-650-codex-smoke-abc7eee-001`.
- Caller class: `agent_runtime`; hierarchy tier: 3.
- Response: `SENTINEL_CODEX_OK`.
- Usage: 6,733 input tokens and 11 output tokens.
- Public rate-card equivalent: USD 0.0013598, source
  `usage_price_table`; this is not represented as a marginal subscription
  charge.
- Request Inspector provider/model/tier/caller lineage: PASS.
- Credential redaction in Inspector and the bounded Gateway journal: PASS.
- Gateway health and readiness: PASS; `NRestarts=0`.

After the bounded request, runtime control returned to the token-free
`local-loop` provider. Full live evidence is retained in issue #650 comment
`5482725491`.
