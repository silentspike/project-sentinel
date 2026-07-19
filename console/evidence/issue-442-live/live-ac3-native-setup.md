# AC-3 Native Setup Interview

Date: 2026-07-18
Node: `10.0.0.241`

Command shape:

```bash
SENTINEL_GAIA_MODEL=haiku \
SENTINEL_GAIA_MAX_BUDGET_USD=0.04 \
SENTINEL_GAIA_SESSION_TIMEOUT_SECS=180 \
SENTINEL_GAIA_CONSOLE_DIR=/tmp/issue442-ac3-schema/console \
  /opt/sentinel/bin/sentinel-gaia-loop setup-interview --prompt '<complete company checklist>'
```

Output:

```text
gaia_session_id=gaia-setup-3d52a59e-a57d-4bc4-b153-917272143fe8
claude_session_id=3d52a59e-a57d-4bc4-b153-917272143fe8
status=succeeded
exit_code=0
input_tokens=18
output_tokens=1439
cache_read_input_tokens=4758
cache_creation_input_tokens=6052
total_cost_usd=0.0197928
tool_call_count=1
permission_denials=0
tool_call_1=/opt/sentinel/bin/sentinel-gaia init --spec-json '<validated GaiaSpec JSON>' --output-dir '<session>/config' --yes --daemon-dry-run --daemon-bin /opt/sentinel/bin/sentinel-daemon --json
daemon_dry_run=true
```

The single generator invocation used `company_type=software_agency`, `shift_model=hybrid`, `conflict_level=0.2`, and nested `culture.mission` / `culture.values`. No generated configuration was applied to the running company.

Artifact validation:

```bash
/opt/sentinel/bin/sentinel-gaia validate --output-dir /tmp/issue442-ac3-schema/console/sessions/gaia-setup-3d52a59e-a57d-4bc4-b153-917272143fe8/config
```

Output:

```text
OK: 4 agents, 11 rooms, total room capacity 108, daemon.max_agents 4, nightrun.max_agent_id 4
files:
agents/AGENT-01-ANDREAS-SCHMITT.toml
agents/AGENT-02-MARTIN-LANG.toml
agents/AGENT-03-SOPHIE-SCHMITT.toml
agents/AGENT-04-MARA-KRAUS.toml
company-context.md
daemon.toml
gaia-spec.toml
nightrun.toml
rooms.toml
company_context_contains_name=true
daemon_active=active
native_claude_processes_after=none
```

The setup turn cost USD 0.0197928.

After the live run exposed that unknown JSON fields could otherwise be ignored, the inline parser was made strict without changing TOML compatibility. The final deployed `sentinel-gaia` binary was verified separately:

```text
sha256=baa44301bfbffc8991d6781e5bdf237032d318d09db4ae92edb98d9435871ad2
valid_agents=2
valid_rooms=10
context_written=true
invalid_exit=1
invalid_output=Error: GaiaSpec contains unknown fields: mission
gaia_loop_active=active
```
