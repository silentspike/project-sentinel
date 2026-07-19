# Issue #442 Review Hardening Evidence

Date: 2026-07-18
Nodes: `10.0.0.241`, `10.0.0.242`

The deploy touched only `sentinel-dashboard-backend`, `sentinel-gaia-loop`, the
console bundle, their units/scripts, persistent dashboard auth, and the native
Claude executable. `sentinel-daemon` was not restarted. `.240` was not
contacted.

## Remote toolchain and verification

```bash
cargo remote -H root@10.0.0.155 -t /tmp/builds -d 1.95.0 -c -- \
  test -p sentinel-gaia-loop -p sentinel-dashboard-backend
cargo remote -H root@10.0.0.155 -t /tmp/builds -d 1.95.0 -c -- \
  clippy --workspace --all-targets -- -D warnings
cargo remote -H root@10.0.0.155 -t /tmp/builds -d 1.95.0 -c -- \
  build -p sentinel-gaia-loop -p sentinel-dashboard-backend --release
```

```text
rustc 1.95.0 (59807616e 2026-04-14)
sentinel-dashboard-backend: 50 unit tests passed; integration suites passed
sentinel-gaia-loop: 20 passed; 0 failed
workspace clippy: Finished dev profile; exit 0
release build: Finished release profile; exit 0
```

The Gaia PR is stacked on the separate Rust 1.95 security-baseline PR. Its diff
against that base contains no toolchain, DEV-010, CI, deploy-preflight, eBPF,
or sandbox files.

## Persistent auth, native client, and private storage

```bash
sudo /opt/sentinel/scripts/init-dashboard-auth.sh
sudo /opt/sentinel/scripts/install-native-claude.sh \
  /home/ubuntu/.local/share/claude/versions/2.1.214
stat -c '%U:%G %a %n' /opt/sentinel/config/dashboard-backend.env \
  /opt/sentinel/bin/claude-2.1.214 /opt/sentinel/data/gaia-console
sha256sum /opt/sentinel/bin/claude-2.1.214
/opt/sentinel/bin/claude --version
```

```text
both nodes: dashboard_auth=generated permissions=0600 owner=root:root
both nodes: root:root 600 /opt/sentinel/config/dashboard-backend.env
both nodes: root:root 755 /opt/sentinel/bin/claude-2.1.214
both nodes: ubuntu:ubuntu 700 /opt/sentinel/data/gaia-console
both nodes: Gaia files with non-0600 mode or directories with non-0700 mode=0
both nodes: 3c029136f7c81f54ed4a38e9d52e655aad536433dbbde50519c8c31bb646ad14
both nodes: 2.1.214 (Claude Code)
both nodes: /opt/sentinel/bin/claude -> /opt/sentinel/bin/claude-2.1.214 (root-owned symlink)
```

The pinned version was also checked against Anthropic's official release
repository on the deploy date:

```bash
gh api repos/anthropics/claude-code/releases/latest \
  --jq '{tag_name,published_at,html_url}'
```

```text
tag_name=v2.1.214
published_at=2026-07-18T01:20:30Z
https://github.com/anthropics/claude-code/releases/tag/v2.1.214
```

Authenticated readback used the persistent secret without printing it:

```text
.241 login_http=200 authenticated=true sessions_http=200
.242 login_http=200 authenticated=true sessions_http=200
```

## Admission and process isolation

These checks used invalid requests or an externally held `flock`; they did not
invoke Claude or spend tokens.

```text
missing_idempotency_http=400 error=Idempotency-Key header is required
foreign_resume_http=400 error=invalid Gaia resume session: expected a local gaia-* session id
single_active_http=429 error=another Gaia Console session is already active
rate_request_7_http=429 body={"error":"Gaia request rate limit exceeded"}
same_key_retry_after_lock_http=400 body={"error":"invalid Gaia resume session: expected a local gaia-* session id"}
claude_processes_after=0
```

Unit readback confirms `UMask=0077`, `KillMode=control-group`, `MemoryMax`,
`TasksMax`, and `CPUQuota` on both services. Remote tests additionally prove
that timeout kills the fake Claude process group, idempotency cannot duplicate
a run, rolling budget exhaustion maps to 429, and concurrent JSONL appends stay
complete and parseable.

## Service and daemon boundary

```text
.241 dashboard=active gaia=active daemon=active
.241 daemon_untouched=yes timestamp=Sat 2026-06-27 17:20:52 UTC
.242 dashboard=active gaia=active daemon=active
.242 daemon_untouched=yes timestamp=Sat 2026-06-27 17:21:26 UTC
both nodes: dashboard/gaia panic or fatal lines since deploy=0
both nodes: exact-name Claude processes after checks=0
```

## Live Claude authentication recovery

One minimal real request was attempted on each node after the pinned native
client was installed. The client started from the hardened service path and
wrote a private stream, but neither VM had usable provider credentials. On
`.241`, `claude auth status` reported `loggedIn=false`; on `.242` it reported a
stored `claude.ai` login, but the real request still received HTTP 401 Invalid
authentication credentials. The idempotent retries returned their respective
same failed sessions and did not launch a second provider request. No tokens or
cost were incurred.

```text
gaia_session_id=gaia-deep-bbec10f4-e4a8-4ecc-92e4-3a90fb73a29b
claude_session_id=bbec10f4-e4a8-4ecc-92e4-3a90fb73a29b
status=failed exit_code=1 total_cost_usd=0.0
retry: same gaia_session_id and claude_session_id
stream: authentication_failed, OAuth session expired and could not be refreshed
.242 status=failed exit_code=1 total_cost_usd=0.0
.242 stream: authentication_failed, API Error: 401 Invalid authentication credentials
claude_processes_after=0
```

After the operator renewed both native Claude Code logins on 2026-07-19,
`claude auth status` reported `loggedIn=true`, `authMethod=claude.ai` on both
nodes. Fresh bounded Deep sessions then succeeded through the authenticated
dashboard API. Repeating each request with the same idempotency key returned
the same Gaia and Claude session IDs without a second provider run.

```text
.241 login_http=200 first_http=200 retry_http=200
.241 status=succeeded exit_code=0 total_cost_usd=0.01895
.241 gaia_session_id=gaia-deep-b3d4bbf8-7595-4657-a186-149bd284930d
.241 same_gaia_session=true same_claude_session=true
.241 stream_http=200 marker_count=2 claude_processes_after=0
.242 login_http=200 first_http=200 retry_http=200
.242 status=succeeded exit_code=0 total_cost_usd=0.003968500000000001
.242 gaia_session_id=gaia-deep-16c772eb-9114-410e-8b34-7a5e559990dd
.242 same_gaia_session=true same_claude_session=true
.242 stream_http=200 marker_count=2 claude_processes_after=0
```

A fresh Setup Interview on `.242` invoked the deterministic generator once,
produced a private config directory, and passed `sentinel-gaia validate`. Its
idempotent retry returned the same successful session.

```text
login_http=200 first_http=200 retry_http=200
status=succeeded exit_code=0 total_cost_usd=0.0451395
same_gaia_session=true same_claude_session=true
gaia_session_id=gaia-setup-d976a6ab-f502-4320-8470-07ec2eb39b30
OK: 2 agents, 10 rooms, total room capacity 104, daemon.max_agents 2, nightrun.max_agent_id 2
generated_files=7 company_context=present claude_processes_after=0
dashboard=active gaia=active daemon=active
```

Post-login live token cost was USD 0.068058 total across the three distinct
sessions. Retries did not add provider cost. Persistent dashboard auth,
token-free API gates, native provider execution, and Setup generation are now
deployed and live-verified.
