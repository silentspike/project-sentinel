# AC-2 Native Deep Session And Resume

Date: 2026-07-18
Node: `10.0.0.241`

The test used native Claude Code 2.1.214 with `haiku`, safe mode, a strict empty MCP configuration, Bash allowlisting, an explicit USD budget, a process timeout, and closed child stdin.

First turn command shape:

```bash
SENTINEL_GAIA_MODEL=haiku SENTINEL_GAIA_MAX_BUDGET_USD=0.03 \
  SENTINEL_GAIA_CONSOLE_DIR=/tmp/issue442-ac2b \
  /opt/sentinel/bin/sentinel-gaia-loop deep --prompt '<create task request>'
```

First turn output:

```text
gaia_session_id=gaia-deep-afb189a0-7730-4165-8466-63de3f23bb74
claude_session_id=afb189a0-7730-4165-8466-63de3f23bb74
status=succeeded
exit_code=0
input_tokens=26
output_tokens=822
cache_read_input_tokens=8704
cache_creation_input_tokens=4812
total_cost_usd=0.0146304
tool_call_1=/opt/sentinel/bin/sentinel-ctl task create ISSUE442-NATIVE-DEEP-AC2B 1 --description native-claude-safe-mode-verification --confirm --json
tool_call_1_status=202 accepted
```

Claude initially supplied the invalid string `gaia-console` to the numeric `--by` option for the assign command. `sentinel-ctl` rejected it before any request. The explicit follow-up resumed the same Claude session with the typed actor id.

Resume command shape:

```bash
SENTINEL_GAIA_MODEL=haiku SENTINEL_GAIA_MAX_BUDGET_USD=0.03 \
  SENTINEL_GAIA_CONSOLE_DIR=/tmp/issue442-ac2b \
  /opt/sentinel/bin/sentinel-gaia-loop deep \
  --resume afb189a0-7730-4165-8466-63de3f23bb74 \
  --prompt '<correct assign and status request>'
```

Resume output:

```text
gaia_session_id=gaia-deep-fa89af6b-7cc0-4c1b-a13b-4faf376f8074
claude_session_id=afb189a0-7730-4165-8466-63de3f23bb74
status=succeeded
exit_code=0
input_tokens=26
output_tokens=500
cache_read_input_tokens=15641
cache_creation_input_tokens=965
total_cost_usd=0.0060201
tool_call_1=/opt/sentinel/bin/sentinel-ctl task assign 2 1 --by 1 --confirm --json
tool_call_1_status=202 accepted
tool_call_2=/opt/sentinel/bin/sentinel-ctl task status 2 in_progress --confirm --json
tool_call_2_status=202 accepted
native_claude_processes_after=none
```

Read-only EventStore verification:

```bash
python3 - <<'PY'
import sqlite3
c=sqlite3.connect('file:/opt/sentinel/data/events.db?mode=ro', uri=True)
for row in c.execute('SELECT id,event_type,aggregate_id,payload FROM events WHERE id BETWEEN 1092292 AND 1092294 ORDER BY id'):
    print(row)
PY
```

Output:

```text
1092292 task_created TASK-2 {"type":"TaskCreated","task_id":2,"title":"ISSUE442-NATIVE-DEEP-AC2B","assigned_to":1}
1092293 task_assigned TASK-2 {"type":"TaskAssigned","task_id":2,"assigned_to":1,"assigned_by":1}
1092294 task_status_changed TASK-2 {"type":"TaskStatusChanged","task_id":2,"old_status":"pending","new_status":"in_progress"}
```

Accepted AC-2 turns cost USD 0.0206505 total. A pre-fix diagnostic run consumed USD 0.0642571 after inherited stdin exposed the need for `Stdio::null()`; that run failed and is excluded from the accepted-session benchmark, but its cost is reported here rather than hidden.
