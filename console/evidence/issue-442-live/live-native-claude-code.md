# Issue #442 Native Claude Code Verification

Date: 2026-07-18

The current native Claude Code client was installed with Anthropic's native installer on both test nodes. The runtime uses the stable `/opt/sentinel/bin/claude` path. Credentials were transferred separately and are not part of this evidence or the repository.

No command in this verification touched `.240`.

The native updater was run immediately before final verification:

```bash
for host in 10.0.0.241 10.0.0.242; do
  ssh ubuntu@$host '/opt/sentinel/bin/claude update; /opt/sentinel/bin/claude --version; readlink -f /opt/sentinel/bin/claude'
done
```

Output on both nodes:

```text
Current version: 2.1.214
Checking for updates to latest version...
Claude Code is up to date (2.1.214)
2.1.214 (Claude Code)
<native-install-root>/versions/2.1.214
```

Command:

```bash
for host in 10.0.0.241 10.0.0.242; do
  ssh ubuntu@$host '/opt/sentinel/bin/claude --version; file -L /opt/sentinel/bin/claude; /opt/sentinel/bin/claude auth status --json | <redacted-field-filter>; command -v node || true; command -v npm || true'
done
```

Output:

```text
HOST=10.0.0.241
2.1.214 (Claude Code)
/opt/sentinel/bin/claude: ELF 64-bit LSB executable, x86-64, dynamically linked
logged_in=true
auth_method=claude.ai
subscription_type=max
node_present=no
npm_present=no
HOST=10.0.0.242
2.1.214 (Claude Code)
/opt/sentinel/bin/claude: ELF 64-bit LSB executable, x86-64, dynamically linked
logged_in=true
auth_method=claude.ai
subscription_type=max
node_present=no
npm_present=no
```

Minimal native smoke output:

```text
10.0.0.241 result=ISSUE442_NATIVE_OK total_cost_usd=0.013909
10.0.0.242 result=ISSUE442_NATIVE_OK total_cost_usd=0.013579
```

The native smoke cost was USD 0.027488 total. These were explicit operator tests; the readiness service did not invoke Claude.

After the final session-spawn hardening, the exact release artifact was tested
again on `.242`. The runner now clears inherited environment variables and
restores only the native-client runtime allowlist plus three explicit Sentinel
runtime variables (console directory and two tool paths). The remote fake-client test proves an injected parent
secret is absent in the child; this live smoke proves native authentication
still works through the allowlisted `HOME`.

```bash
SENTINEL_GAIA_CONSOLE_DIR=/tmp/issue442-final-native-smoke \
SENTINEL_GAIA_CLAUDE_BIN=/opt/sentinel/bin/claude \
SENTINEL_GAIA_MODEL=haiku \
SENTINEL_GAIA_MAX_BUDGET_USD=0.02 \
SENTINEL_GAIA_SESSION_TIMEOUT_SECS=60 \
/tmp/issue442-final/sentinel-gaia-loop deep \
  --prompt 'Reply exactly ISSUE442_ENV_CLEAR_OK. Do not call tools.'
```

Output:

```text
binary_sha256=569089bc92c851182d9784d329af9b41295e4b3e2e6507f3581f4445a649d410
claude_version=2.1.214
status=succeeded
exit_code=0
input_tokens=4074
output_tokens=73
total_cost_usd=0.004439
stream_marker=ISSUE442_ENV_CLEAR_OK
stderr=empty
claude_processes_after=0
```

Accepted native-client verification cost is USD 0.0767773 in total across the
two installation smokes, Deep/resume AC, Setup AC, dashboard-stream AC, and this
final hardening smoke. The failed pre-fix diagnostic cost of USD 0.0642571 is
reported separately and excluded from accepted-session metrics.
