# AC-4 Authenticated Dashboard Stream

Date: 2026-07-18
Node: `10.0.0.241`

A random temporary operator key and `haiku`/USD 0.03 cap were installed as a test-only systemd drop-in. The key was never printed and the drop-in was removed immediately after the run.

Command shape:

```bash
curl -sk -c cookies -X POST https://127.0.0.1:8001/api/auth/login -d '<temporary key JSON>'
curl -sk -b cookies -X POST https://127.0.0.1:8001/api/gaia/deep \
  -H 'content-type: application/json' \
  -d '{"prompt":"Reply exactly ISSUE442_AC4_STREAM_OK. Do not call tools."}'
curl -sk -b cookies https://127.0.0.1:8001/api/gaia/sessions
curl -sk -b cookies https://127.0.0.1:8001/api/gaia/sessions/<id>/stream
```

Output:

```text
login_http=200
authenticated=true
deep_http=200
gaia_session_id=gaia-deep-f5551e08-5dd3-40f2-8849-866a95663771
kind=deep
status=succeeded
exit_code=0
total_cost_usd=0.004407
sessions_http=200
stream_http=200
session_index_count=1
session_visible=true
stream_tool_calls=0
stream_result=ISSUE442_AC4_STREAM_OK
dashboard_active=active
daemon_active=active
temporary_dropin_removed=yes
temporary_key_absent=yes
native_claude_processes_after=none
dashboard_panic_fatal=none
```

The authenticated Gaia Console was also verified in a real browser against the
deployed HTTPS backend:

```bash
cd console && bun issue442-live-screenshot.mjs
```

Output:

```text
title=Sentinel Gaia-Konsole
alerts=2
sessions=1
stream_contains_marker=true
screenshot=evidence/issue-442-live/gaia-console-live-241.png
```

The committed 760x1200 screenshot was visually inspected. The readiness alerts,
successful Deep session, cost, and streamed marker are visible without overlap.

Guardrail source-diff check:

```bash
git diff --name-only origin/main...HEAD | rg 'snapshot|llm_analy|orchestrator|sentinel-daemon' || true
```

Output:

```text
```

The PR does not modify `llm_analyzer`, the daemon orchestrator, the ECS tick loop, or `WorldSnapshot`. The token-free escalation path remains the AC-1 readiness alert path; it does not spawn a second LLM loop.

The test node's existing daemon configuration has the embedded Platform LLM Analyzer disabled. This was true before and after #442 and is reported as the actual runtime state rather than described as active:

```text
Platform LLM Analyzer deaktiviert
Platform LLM Analyzer initialisiert enabled=false
sentinel-daemon.service=active
daemon_source_diff_lines=0
```
