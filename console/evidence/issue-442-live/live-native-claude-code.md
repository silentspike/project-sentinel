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
