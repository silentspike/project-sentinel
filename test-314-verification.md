# Issue #314 Verification

Stand: `2026-04-24`

Branch: `feat/issue-314-agent-model-policy`

## Task 1 - Phase 1: Issue-Body-Repair, Branch und Preflight

### AC-1 - Branch basiert sauber auf origin/main

Command:

```bash
git status --short --branch
git rev-parse HEAD
git rev-parse origin/main
git rev-list --left-right --count HEAD...origin/main
```

Output:

```text
## main...origin/main
0f1c46c19bfa61d0616b3468834d29b557b3e254
0f1c46c19bfa61d0616b3468834d29b557b3e254
0	0
```

Nach Branch-Erstellung:

```text
Switched to a new branch 'feat/issue-314-agent-model-policy'
```

PASS: Branch wurde von synchronem `main` bei `0f1c46c19bfa61d0616b3468834d29b557b3e254` erstellt.

### AC-2 - GitHub-Issue-Body ist spec-ready

Command:

```bash
gh issue edit 314 --repo silentspike/project-sentinel \
  --body-file docs/issue-314-body.md \
  --remove-label "quality:needs-spec" \
  --remove-label "status:triage" \
  --remove-label "status:backlog" \
  --add-label "quality:ready" \
  --add-label "status:in-progress"

gh issue view 314 --repo silentspike/project-sentinel --json number,title,state,labels,body,updatedAt
```

Output excerpt:

```text
https://github.com/silentspike/project-sentinel/issues/314
labels: quality:ready, status:in-progress, type:feature, comp:cortex, comp:inference, ...
body contains: Kontext, Scope, Out of Scope, Acceptance Criteria, Benchmarks, Verify-Ideen
updatedAt: 2026-04-24T05:51:12Z
```

PASS: Issue-Body enthaelt die vom Quality-Gate geforderten Sektionen `Scope`, `Out of Scope` und `Benchmarks`.

### AC-3 - Labels sind repariert

Command:

```bash
gh issue view 314 --repo silentspike/project-sentinel --json labels
```

Output excerpt:

```text
quality:ready
status:in-progress
```

PASS: `quality:needs-spec`, `status:triage` und `status:backlog` wurden entfernt; `quality:ready` und `status:in-progress` sind gesetzt.

### AC-4 - Haiku-Provider-String ist live geprueft

Command:

```bash
ssh ubuntu@10.0.0.240 "/usr/bin/claude -p --model haiku 'Antworte exakt mit PONG.'"
```

Output:

```text
PONG
```

PASS: Der aktuelle `claude-code` Pfad akzeptiert `--model haiku` auf der VM.

### AC-5 - Kein Daemon-Code in Task 1 geaendert

Command:

```bash
git status --short --branch
```

Output:

```text
## feat/issue-314-agent-model-policy
 M PROGRESS.md
?? test-314-verification.md
```

Hinweis: `docs/` ist im Repo ignoriert. Der Issue-Body wurde nach dem GitHub-Update als
getracktes Root-Artefakt `issue-314-body.md` gesichert.

PASS: Task 1 aendert nur Tracking-/Dokumentationsartefakte fuer #314, keinen Daemon-Code.

## Task 2 - Phase 2: Gateway Policy-Layer

### AC-1 - Request-Klassen sind modelliert

Command:

```bash
nl -ba cmd/cortex-gateway/internal/proxy/policy.go | sed -n '1,50p'
```

Output excerpt:

```text
RequestClassExternalCompat       RequestClass = "external_compat"
RequestClassAgentRuntime         RequestClass = "agent_runtime"
RequestClassPlatformControlplane RequestClass = "platform_controlplane"
RequestClassServiceInternal      RequestClass = "service_internal"
RequestClassInternalOther        RequestClass = "internal_other"
```

PASS: Die fuenf geplanten Request-Klassen sind zentral im Gateway modelliert.

### AC-2 - Agent-Runtime-Klassifikation ist strikt

Command:

```bash
nl -ba cmd/cortex-gateway/internal/proxy/policy.go | sed -n '26,49p'
```

Output excerpt:

```text
if isAnthropicMessagesPath(path) { return RequestClassExternalCompat }
if platform_analysis == "true" || agent_name == "PLATFORM-CONTROLPLANE" { return RequestClassPlatformControlplane }
if request_type != "" || isServiceIdentity(agentName) { return RequestClassServiceInternal }
if isPositiveNumericAgentID(metadata["agent_id"]) { return RequestClassAgentRuntime }
return RequestClassInternalOther
```

PASS: `agent_runtime` wird erst nach Platform-/Service-/Analyse-Ausschluessen und nur bei positiver numerischer `agent_id` gesetzt.

### AC-3 - Leeres Runtime-Modell wird nur fuer agent_runtime zu Haiku

Command:

```bash
nl -ba cmd/cortex-gateway/internal/proxy/policy.go | sed -n '57,76p'
nl -ba cmd/cortex-gateway/internal/proxy/pipeline.go | sed -n '563,577p'
```

Output excerpt:

```text
if class != RequestClassAgentRuntime { return ModelPolicyResolution{Source: PolicySourceProviderDefault}, nil }
...
return ModelPolicyResolution{Model: model, Source: PolicySourceAgentRuntime}, nil
...
req.Model = policyResolution.Model
req.EffectiveModel = policyResolution.Model
req.PolicySource = policyResolution.Source
```

PASS: Die Policy wird nur fuer `agent_runtime` angewandt und vor dem Provider-Forward in den Request geschrieben.

### AC-4 - Explizites Request-Modell gewinnt

Command:

```bash
nl -ba cmd/cortex-gateway/internal/proxy/policy.go | sed -n '57,64p'
```

Output excerpt:

```text
if model := strings.TrimSpace(explicitModel); model != "" {
    return ModelPolicyResolution{Model: model, Source: PolicySourceRequestOverride}, nil
}
```

PASS: Ein explizites Request-Modell wird nicht von der Agent-Runtime-Policy ueberschrieben.

### AC-5 - /v1/messages bleibt externe Compatibility-Klasse

Command:

```bash
nl -ba cmd/cortex-gateway/internal/proxy/anthropic_api.go | sed -n '86,99p'
nl -ba cmd/cortex-gateway/internal/proxy/policy.go | sed -n '26,29p'
```

Output excerpt:

```text
PreferredProvider: "anthropic-direct"
...
if isAnthropicMessagesPath(path) {
    return RequestClassExternalCompat
}
```

PASS: `/v1/messages` bleibt `external_compat` und bevorzugt weiter `anthropic-direct`.

### AC-6 - Ungueltige Policy/Provider-Kombination ist fail-closed

Command:

```bash
nl -ba cmd/cortex-gateway/internal/proxy/policy.go | sed -n '88,99p'
nl -ba cmd/cortex-gateway/internal/proxy/pipeline.go | sed -n '563,573p'
```

Output excerpt:

```text
case "claude-code", "mock":
    return "haiku", nil
default:
    return "", fmt.Errorf("agent_runtime_model_policy %q is not supported for provider %q", policy, providerName)
...
ph.writeRequestError(w, &req, "model policy rejected", http.StatusUnprocessableEntity)
```

PASS: Nicht unterstuetzte Provider fallen nicht still auf Opus zurueck, sondern erzeugen einen Fehler vor dem Provider-Call.

### Compile-/Regression-Check

Command:

```bash
go test ./cmd/cortex-gateway/internal/proxy ./cmd/cortex-gateway/internal/control
go build ./cmd/cortex-gateway
```

Output:

```text
ok  	github.com/obtFusi/project-sentinel/cmd/cortex-gateway/internal/proxy	1.070s
ok  	github.com/obtFusi/project-sentinel/cmd/cortex-gateway/internal/control	(cached)
```

`go build ./cmd/cortex-gateway` exited `0`.

## Task 3 - Phase 3: Observability und Response Log

### AC-1 - Traffic-Stats zeigen Agent-Runtime-Policy

Command:

```bash
nl -ba cmd/cortex-gateway/main.go | sed -n '287,328p'
```

Output excerpt:

```text
trafficConfig := controlConfig.Get()
lastAgentRuntime, hasLastAgentRuntime := responseLogs.LastByClass(proxy.RequestClassAgentRuntime)
...
"agent_runtime_model_policy":  trafficConfig.AgentRuntimeModelPolicy,
...
stats["last_agent_runtime_effective_model"] = lastAgentRuntime.Model
stats["last_agent_runtime_policy_source"] = lastAgentRuntime.PolicySource
stats["last_agent_runtime_provider"] = lastAgentRuntime.Provider
```

PASS: `/control/traffic-stats` kann die aktive Agent-Runtime-Policy und den letzten effektiven Runtime-Forward ausgeben.

### AC-2 - ResponseLogEntry enthaelt redigierte Policy- und Agent-Felder

Command:

```bash
nl -ba cmd/cortex-gateway/internal/proxy/response_log.go | sed -n '8,19p'
nl -ba cmd/cortex-gateway/internal/proxy/pipeline.go | sed -n '1233,1247p'
```

Output excerpt:

```text
RequestClass RequestClass `json:"request_class,omitempty"`
Provider     string       `json:"provider"`
Model        string       `json:"model,omitempty"`
PolicySource string       `json:"policy_source,omitempty"`
AgentID      string       `json:"agent_id,omitempty"`
AgentName    string       `json:"agent_name,omitempty"`
Content      string       `json:"content"`
```

PASS: Response-Logs enthalten Request-Klasse, Provider, effektives Modell, Policy-Source und Agent-Metadaten, aber keine Header-/Secret-Felder.

### AC-3 - Success-, Stream- und Error-Logs enthalten Policy-Felder

Command:

```bash
rg -n '"request_class"|"effective_model"|"policy_source"' cmd/cortex-gateway/internal/proxy/pipeline.go
```

Output excerpt:

```text
454: "request_class", req.RequestClass,
455: "effective_model", "sentinel-synth-v1",
456: "policy_source", req.PolicySource,
614: "request_class", req.RequestClass,
615: "effective_model", effectiveModelForLog(&req, ""),
616: "policy_source", req.PolicySource,
733: "request_class", req.RequestClass,
734: "effective_model", effectiveModelForLog(&req, resp.Model),
735: "policy_source", req.PolicySource,
1388: "request_class", req.RequestClass,
1389: "effective_model", effectiveModelForLog(req, ""),
1390: "policy_source", req.PolicySource,
1416: "request_class", req.RequestClass,
1417: "effective_model", effectiveModelForLog(req, ""),
1418: "policy_source", req.PolicySource,
```

PASS: Synthesis/APICP, Provider-Error, Provider-Success, Stream-Error und Stream-Success sind observierbar.

### AC-4 - Keine Secrets in Observability-Feldern

Command:

```bash
rg -n 'ResponseLogEntry|agent_runtime_model_policy|last_agent_runtime|x-api-key|authorization|secret|token' \
  cmd/cortex-gateway/main.go \
  cmd/cortex-gateway/internal/proxy/response_log.go \
  cmd/cortex-gateway/internal/proxy/pipeline.go
```

Output excerpt:

```text
cmd/cortex-gateway/main.go:312: "agent_runtime_model_policy": trafficConfig.AgentRuntimeModelPolicy,
cmd/cortex-gateway/main.go:324: stats["last_agent_runtime_policy_source"] = lastAgentRuntime.PolicySource
cmd/cortex-gateway/internal/proxy/response_log.go:8:type ResponseLogEntry struct {
```

PASS: Die neuen Stats-/ResponseLog-Felder speichern keine Authorization-Header, API-Keys oder Secret-Werte.

### AC-5 - Response-Log-Append ist bounded und ohne steady-state Slice-Kopie

Command:

```bash
nl -ba cmd/cortex-gateway/internal/proxy/response_log.go | sed -n '25,55p'
```

Output excerpt:

```text
return &ResponseLogBuffer{limit: limit, entries: make([]ResponseLogEntry, 0, limit)}
...
if len(b.entries) < b.limit {
    b.entries = append(b.entries, entry)
    return
}

b.entries[b.next] = entry
b.next = (b.next + 1) % b.limit
```

PASS: Der Buffer ist begrenzt und ersetzt nach Erreichen des Limits Eintraege per Ring-Index statt den Restslice zu kopieren.

### Compile-/Regression-Check

Command:

```bash
gofmt -w cmd/cortex-gateway/main.go cmd/cortex-gateway/internal/proxy/pipeline.go cmd/cortex-gateway/internal/proxy/response_log.go
go test ./cmd/cortex-gateway/internal/proxy ./cmd/cortex-gateway/internal/control
go build ./cmd/cortex-gateway
```

Output:

```text
ok  	github.com/obtFusi/project-sentinel/cmd/cortex-gateway/internal/proxy	(cached)
ok  	github.com/obtFusi/project-sentinel/cmd/cortex-gateway/internal/control	(cached)
```

`go build ./cmd/cortex-gateway` exited `0`.

PASS: Betroffene Gateway-Packages testen und bauen nach Task 3.

### Zwischenfund

Der erste `go test`-Lauf zeigte, dass die Policy zu frueh vor dem Synthesis-Pfad lief und Testprovider-Forwards blockierte. Fix:

- Policy-Anwendung wurde hinter Synthesis/API-CP/Sequencing und direkt vor Streaming/Provider.Send verschoben.
- Testprovider `mock` wird als Testmapping fuer `haiku` akzeptiert; echte unbekannte Provider bleiben fail-closed.

## Task 4 - Phase 4: Go-Tests

### AC-1 - Agent-Runtime-Klassifikation ist getestet

Command:

```bash
go test ./cmd/cortex-gateway/internal/proxy -run 'TestClassifyRequestStrictAgentRuntime|TestResolveModelPolicy'
```

Output:

```text
ok  	github.com/obtFusi/project-sentinel/cmd/cortex-gateway/internal/proxy	0.033s
```

Covered cases:

```text
/v1/messages + agent_id -> external_compat
platform_analysis/request_type/service identity + agent_id -> not agent_runtime
positive numeric agent_id -> agent_runtime
0, leading zero, non-numeric agent_id -> internal_other
```

PASS: `agent_runtime` ist streng positiv-numerisch und wird erst nach Platform-/Service-Ausschluessen gesetzt.

### AC-2 - /v1/messages bleibt externe Compatibility-Klasse

Command:

```bash
go test ./cmd/cortex-gateway/internal/proxy -run TestPipelineAnthropicMessagesPassthrough
```

Output:

```text
ok  	github.com/obtFusi/project-sentinel/cmd/cortex-gateway/internal/proxy	0.019s
```

Assertions:

```text
claude-code calls = 0
anthropic-direct calls = 1
PreferredProvider = anthropic-direct
RequestClass = external_compat
PolicySource = request_override
```

PASS: `/v1/messages` laeuft nicht in die Agent-Runtime-Haiku-Policy.

### AC-3 - Agent-Runtime-Haiku und Request-Override sind getestet

Command:

```bash
go test ./cmd/cortex-gateway/internal/proxy -run 'TestPipelineAgentRuntimeAppliesHaikuPolicy|TestResolveModelPolicy'
```

Output:

```text
ok  	github.com/obtFusi/project-sentinel/cmd/cortex-gateway/internal/proxy	0.032s
```

Assertions:

```text
agent_runtime without model -> model haiku, effective_model haiku, policy_source agent_runtime_policy
explicit request model -> policy_source request_override
```

PASS: Der Runtime-Pfad setzt Haiku nur bei leerem Modell; explizite Modelle gewinnen.

### AC-4 - Fail-Closed fuer ungueltige Policy/Provider-Kombination

Command:

```bash
go test ./cmd/cortex-gateway/internal/proxy -run TestResolveModelPolicy
```

Output:

```text
ok  	github.com/obtFusi/project-sentinel/cmd/cortex-gateway/internal/proxy	0.023s
```

Assertions:

```text
provider anthropic-direct + agent_runtime_policy haiku -> error contains "not supported"
policy opus -> error contains "unknown"
```

PASS: Unaufloesbare Policy-Zustaende fallen nicht still auf Provider-Default/Opus zurueck.

### AC-5 - Control-Config-Default und Validation sind getestet

Command:

```bash
go test ./cmd/cortex-gateway/internal/control -run 'TestNewConfig_Defaults|TestConfig_Update_AgentRuntimeModelPolicy'
```

Output:

```text
ok  	github.com/obtFusi/project-sentinel/cmd/cortex-gateway/internal/control	0.010s
```

Assertions:

```text
default agent_runtime_model_policy = haiku
accepted: "haiku", ""
rejected: "opus", non-string
```

PASS: Die Runtime-Policy ist im Control-Plane-State deterministisch validiert.

### AC-6 - ResponseLogBuffer-Ring und LastByClass sind getestet

Command:

```bash
go test ./cmd/cortex-gateway/internal/proxy -run TestResponseLogBufferRingOverwriteAndLastByClass
```

Output:

```text
ok  	github.com/obtFusi/project-sentinel/cmd/cortex-gateway/internal/proxy	0.017s
```

Assertions:

```text
limit 2 -> third Add overwrites oldest entry
Entries() returns chronological [agent-1, agent-2]
LastByClass(agent_runtime) returns agent-2
LastByClass(external_compat) does not return overwritten external entry
```

PASS: Der bounded Ringbuffer ist regressionsgesichert.

### Compile-/Regression-Check

Command:

```bash
gofmt -w cmd/cortex-gateway/internal/proxy/policy_test.go cmd/cortex-gateway/internal/proxy/response_log_test.go cmd/cortex-gateway/internal/proxy/pipeline_test.go cmd/cortex-gateway/internal/control/plane_test.go
go test ./cmd/cortex-gateway/internal/proxy ./cmd/cortex-gateway/internal/control
go build ./cmd/cortex-gateway
```

Output:

```text
ok  	github.com/obtFusi/project-sentinel/cmd/cortex-gateway/internal/proxy	1.087s
ok  	github.com/obtFusi/project-sentinel/cmd/cortex-gateway/internal/control	0.019s
```

`go build ./cmd/cortex-gateway` exited `0`.

## Task 5 - Phase 5: Benchmarks

### AC-1 bis AC-4 - Benchmark-Zielwerte und Allokationen

Command:

```bash
rm -f /tmp/issue314-vmstat.txt /tmp/issue314-bench.txt
(vmstat 1 5 > /tmp/issue314-vmstat.txt &)
/usr/bin/time -v go test ./cmd/cortex-gateway/internal/proxy \
  -bench 'Benchmark(ClassifyRequest|ResolveModelPolicy|ResponseLogBufferAdd)' \
  -benchmem \
  -run '^$' 2>&1 | tee /tmp/issue314-bench.txt
wait
cat /tmp/issue314-vmstat.txt
```

Output:

```text
goos: linux
goarch: amd64
pkg: github.com/obtFusi/project-sentinel/cmd/cortex-gateway/internal/proxy
cpu: AMD Ryzen 9 5900HS with Radeon Graphics
BenchmarkClassifyRequestAgentRuntime-16       	 3408093	       494.9 ns/op	      16 B/op	       1 allocs/op
BenchmarkResolveModelPolicyAgentRuntime-16    	28704816	        38.43 ns/op	       0 B/op	       0 allocs/op
BenchmarkResponseLogBufferAdd-16              	  331939	      3831 ns/op	       0 B/op	       0 allocs/op
PASS
ok  	github.com/obtFusi/project-sentinel/cmd/cortex-gateway/internal/proxy	4.537s
```

PASS:

```text
ClassifyRequest: 494.9 ns/op < 1us/op
ResolveModelPolicy: 38.43 ns/op < 1us/op
ResponseLogBuffer.Add: 3831 ns/op < 10us/op
benchmem visible: B/op and allocs/op recorded
```

### AC-5 - System-Monitoring waehrend Benchmarks

Command:

```bash
/usr/bin/time -v go test ./cmd/cortex-gateway/internal/proxy \
  -bench 'Benchmark(ClassifyRequest|ResolveModelPolicy|ResponseLogBufferAdd)' \
  -benchmem \
  -run '^$'
vmstat 1 5
iostat -dx 1 2
```

Output excerpt:

```text
User time (seconds): 6.31
System time (seconds): 2.06
Percent of CPU this job got: 125%
Elapsed (wall clock) time: 0:06.66
Maximum resident set size (kbytes): 245816
Swaps: 0
File system inputs: 49904
File system outputs: 112
```

`vmstat` excerpt:

```text
procs -----------memory---------- ---swap-- -----io---- -system-- -------cpu-------
 r  b   swpd   free   buff  cache   si   so    bi    bo   in   cs us sy id wa st gu
13  0 7093336 508428  4292 5292392 111  262  7410  7235 14249  20 24  8 67  1  0  0
 7  0 7093904 449476  4292 5194228   0  540 203676 91808 44259 36959 34 29 36 1 0 0
 9  1 7096252 508796  4292 5326296 452 2792 211448 46760 40951 43288 33 21 46 1 0 0
```

`iostat` excerpt:

```text
Device            r/s     rkB/s     w/s     wkB/s  r_await w_await aqu-sz  %util
nvme0n1        187.00   2984.00 1799.00  24092.00     0.07    2.95   5.35   5.80
```

PASS: CPU/RAM/IO-Metriken wurden dokumentiert; der Benchmarklauf beendet sauber ohne Swap-Fehler oder IO-Blockade.

## Task 6 - Phase 6: Gateway Deploy auf 10.0.0.240

### AC-1 - Deploy trifft den systemd-Binary-Pfad

Command:

```bash
ssh ubuntu@10.0.0.240 "systemctl cat sentinel-gateway --no-pager && systemctl is-active sentinel-gateway && ls -l /opt/sentinel/bin/cortex-gateway"
```

Output excerpt:

```text
ExecStart=/opt/sentinel/bin/cortex-gateway
active
-rwxr-xr-x 1 root root 23269163 Apr  3 22:39 /opt/sentinel/bin/cortex-gateway
```

PASS: Der Zielpfad ist `/opt/sentinel/bin/cortex-gateway`.

### AC-2 - Linux-Binary wurde deployed und Service ist aktiv

Command:

```bash
GOOS=linux GOARCH=amd64 go build -o cortex-gateway ./cmd/cortex-gateway/
scp cortex-gateway ubuntu@10.0.0.240:/tmp/cortex-gateway.issue314
ssh ubuntu@10.0.0.240 "set -euo pipefail; \
  sudo systemctl stop sentinel-gateway; \
  sudo cp /tmp/cortex-gateway.issue314 /opt/sentinel/bin/cortex-gateway; \
  sudo chmod 0755 /opt/sentinel/bin/cortex-gateway; \
  sudo chown root:root /opt/sentinel/bin/cortex-gateway; \
  sudo systemctl start sentinel-gateway; \
  sleep 2; \
  systemctl is-active sentinel-gateway; \
  ls -l /opt/sentinel/bin/cortex-gateway"
```

Output:

```text
active
-rwxr-xr-x 1 root root 23244031 Apr 24 06:24 /opt/sentinel/bin/cortex-gateway
```

Zwischenfund:

```text
cp: cannot create regular file '/opt/sentinel/bin/cortex-gateway': Text file busy
```

Fix: Service wurde vor dem finalen Copy gestoppt und danach wieder gestartet.

Backup:

```text
/opt/sentinel/bin/cortex-gateway.bak-issue314-20260424062401
```

PASS: Neues Binary ist live am systemd-Pfad, Gateway-Service ist aktiv.

### AC-3 - Health ist OK

Command:

```bash
ssh ubuntu@10.0.0.240 "curl -s localhost:8080/health"
```

Output:

```json
{"status":"ok","version":"0.1.0","circuit_breakers":{},"guardrails_enabled":false}
```

PASS: Gateway-Health liefert `status=ok`.

### AC-4 - Traffic-Stats zeigen neues Policy-Feld live

Command:

```bash
ssh ubuntu@10.0.0.240 "curl -s localhost:8081/control/traffic-stats | python3 -m json.tool | sed -n '1,120p'"
```

Output excerpt:

```json
{
    "active_forward_calls": 0,
    "active_patterns": 123,
    "agent_runtime_model_policy": "haiku",
    "apicp_enabled": true,
    "primary_provider": "claude-code",
    "response_log_entries": 0,
    "synthesis_enabled": true
}
```

PASS: Die neue Gateway-Policy ist auf der VM sichtbar.

### AC-5 - Journal zeigt keinen Startfehler

Command:

```bash
ssh ubuntu@10.0.0.240 "pid=\$(pgrep -n cortex-gate); echo PID=\$pid; journalctl _PID=\$pid --since '2 min ago' --no-pager | tail -80"
```

Output excerpt:

```text
PID=1578506
cortex-gateway starting
registered provider name=anthropic-direct model=claude-opus-4-6
registered provider name=claude-code model=claude-opus-4-6
traffic control defaults applied
company context loaded
proxy server starting addr=:8080
control plane starting addr=:8081
```

PASS: PID-basiertes Journal zeigt normalen Startup ohne Panic/Fatal.

## Task 7 - Phase 8: AC-Matrix und Live-Verifikation

### AC-1 - Interne agent_runtime Requests bekommen effektiv Haiku

Command:

```bash
ssh ubuntu@10.0.0.240 "cat > /tmp/issue314-agent-runtime.json <<'JSON'
{
  \"messages\": [{\"role\": \"user\", \"content\": \"Antworte exakt mit PONG314.\"}],
  \"metadata\": {
    \"agent_id\": \"314\",
    \"agent_name\": \"Issue314 Test Agent\",
    \"agent_role\": \"Testperson\",
    \"room_id\": \"buero-ceo\",
    \"is_directly_addressed\": \"true\"
  }
}
JSON
timeout 90 curl -sS -w '\nHTTP_STATUS=%{http_code}\n' \
  -X POST http://127.0.0.1:8080/internal/llm \
  -H 'Content-Type: application/json' \
  --data-binary @/tmp/issue314-agent-runtime.json"
```

Output:

```json
{"content":"PONG314","model":"haiku","provider":"claude-code","tokens_used":19282,"input_tokens":18783,"output_tokens":499,"finish_reason":"success","actions":[{"type":"chat","content":"PONG314","emotion":"neutral"}],"request_id":"49c25b39-466d-4c48-9bc0-04fac9e9b360"}
```

```text
HTTP_STATUS=200
```

PASS: Der interne Agent-Runtime-Forward lief ueber `claude-code` mit effektivem Modell `haiku`.

### AC-2 - Daemon pinnt kein AGENT_MODEL_HAIKU

Command:

```bash
rg -n "AGENT_MODEL_HAIKU" services/sentinel-daemon/src || true
rg -n "model:\s*String::new\(\)|model:\s*\"haiku\"|model:\s*AGENT" \
  services/sentinel-daemon/src/llm_bridge.rs \
  services/sentinel-daemon/src/orchestrator.rs \
  services/sentinel-daemon/src/platform_controlplane/llm_analyzer.rs
nl -ba services/sentinel-daemon/src/llm_bridge.rs | sed -n '600,620p'
```

Output:

```text
services/sentinel-daemon/src/platform_controlplane/llm_analyzer.rs:326:        model: String::new(),
services/sentinel-daemon/src/llm_bridge.rs:612:            model: String::new(), // Gateway waehlt default
```

```text
605	        GatewayRequest {
606	            messages: vec![GatewayMessage {
607	                role: "user".to_string(),
608	                content: user_prompt,
609	            }],
610	            temperature: 0.7,
611	            max_tokens: 1024,
612	            model: String::new(), // Gateway waehlt default
613	            metadata,
614	        }
```

PASS: Kein `AGENT_MODEL_HAIKU`; Agent-Runtime bleibt beim Gateway-/Policy-Default.

### AC-3 - VM-Evidence zeigt echten agent_runtime Forward mit Haiku

Command:

```bash
ssh ubuntu@10.0.0.240 "curl -s localhost:8081/control/traffic-stats | python3 -m json.tool | sed -n '1,140p'"
ssh ubuntu@10.0.0.240 "curl -s localhost:8081/control/traffic-responses | python3 -c 'import sys,json; data=json.load(sys.stdin); [print(json.dumps({k:e.get(k) for k in (\"request_id\",\"request_class\",\"provider\",\"model\",\"policy_source\",\"agent_id\",\"agent_name\")}, ensure_ascii=False)) for e in data if e.get(\"request_id\")==\"49c25b39-466d-4c48-9bc0-04fac9e9b360\"]'"
```

Output excerpt:

```json
{
    "agent_runtime_model_policy": "haiku",
    "last_agent_runtime_effective_model": "haiku",
    "last_agent_runtime_policy_source": "agent_runtime_policy",
    "last_agent_runtime_provider": "claude-code"
}
```

```json
{"request_id": "49c25b39-466d-4c48-9bc0-04fac9e9b360", "request_class": "agent_runtime", "provider": "claude-code", "model": "haiku", "policy_source": "agent_runtime_policy", "agent_id": "314", "agent_name": "Issue314 Test Agent"}
```

PASS: Live-Observability belegt den echten Forward mit `agent_runtime` + `haiku`.

### AC-4 - /v1/messages bleibt externer Compatibility-Pfad

Command:

```bash
ssh ubuntu@10.0.0.240 "cat > /tmp/issue314-v1messages.json <<'JSON'
{
  \"model\": \"claude-opus-4-6\",
  \"max_tokens\": 16,
  \"messages\": [{\"role\": \"user\", \"content\": [{\"type\": \"text\", \"text\": \"Sag PONG314\"}]}]
}
JSON
timeout 30 curl -sS -w '\nHTTP_STATUS=%{http_code}\n' \
  -X POST http://127.0.0.1:8080/v1/messages \
  -H 'Content-Type: application/json' \
  -H 'x-api-key: dummy-issue314' \
  -H 'anthropic-version: 2023-06-01' \
  --data-binary @/tmp/issue314-v1messages.json"
```

Output:

```json
{"type":"error","error":{"type":"authentication_error","message":"provider request failed"}}
```

```text
HTTP_STATUS=401
```

Journal:

```bash
ssh ubuntu@10.0.0.240 "pid=\$(pgrep -n cortex-gate); journalctl _PID=\$pid --since '2 min ago' --no-pager | grep -E 'provider request failed|anthropic-direct|external_compat|request_override' | tail -20"
```

Output:

```text
provider request failed provider=anthropic-direct request_class=external_compat effective_model=claude-opus-4-6 policy_source=request_override agent_id="" agent_name="" error="provider error: HTTP 401: ... invalid x-api-key ..."
```

PASS: `/v1/messages` geht weiter an `anthropic-direct` als `external_compat`; die interne Agent-Policy greift dort nicht.

### AC-5 - Observability redigiert und ohne Secrets

Command:

```bash
ssh ubuntu@10.0.0.240 "curl -s localhost:8081/control/traffic-stats | grep -E 'x-api-key|dummy-issue314|authorization|Bearer|sk-ant|api_key' || true; curl -s localhost:8081/control/traffic-responses | grep -E 'dummy-issue314|Bearer|sk-ant|api_key|authorization' || true"
ssh ubuntu@10.0.0.240 "pid=\$(pgrep -n cortex-gate); journalctl _PID=\$pid --since '10 min ago' --no-pager | grep -E 'dummy-issue314|Bearer|sk-ant|api_key|authorization' || true"
```

Output:

```text

```

PASS: Stats, Response-Log und PID-Journal enthalten keine Dummy-Key-/Bearer-/Secret-Werte.

### AC-6 - Tests und VM-Smoke belegen interne/externe Trennung

Command:

```bash
go test ./cmd/cortex-gateway/internal/proxy ./cmd/cortex-gateway/internal/control
go build ./cmd/cortex-gateway
ssh ubuntu@10.0.0.240 "journalctl -u sentinel-gateway --since '10 min ago' --no-pager | grep -iE 'panic|fatal|segfault' || true; printf '\n--- daemon drift/panic ---\n'; journalctl -u sentinel-daemon --since '10 min ago' --no-pager | grep -iE 'panic|drift' || true"
```

Output:

```text
ok  	github.com/obtFusi/project-sentinel/cmd/cortex-gateway/internal/proxy	(cached)
ok  	github.com/obtFusi/project-sentinel/cmd/cortex-gateway/internal/control	(cached)

--- daemon drift/panic ---
```

PASS: Unit-/Regressionstests, Gateway-Build, VM-Smoke und Panic/Drift-Grep sind gruen.

## Task 8 - Dokumentation, PR- und Close-Sequenz

### AC-1 - CHANGELOG enthaelt #314

Command:

```bash
rg -n "#314|Gateway Model Policy|agent_runtime_model_policy|Response-Log-Buffer" CHANGELOG.md
```

Output:

```text
12:- **Gateway Model Policy: Agent-Runtime nutzt ueber Inference-Layer standardmaessig Haiku** (#314)
14:  - `agent_runtime_model_policy` setzt im Gateway-Control-State standardmaessig `haiku`, ohne den Daemon oder `/v1/messages` hart zu pinnen
18:  - Response-Log-Buffer nutzt jetzt einen bounded circular buffer statt steady-state Slice-Kopie
```

PASS: `CHANGELOG.md` dokumentiert #314 unter `[Unreleased]`.

### AC-2 - PR-Pflichtsektionen vorbereitet

Pflichtsektionen fuer `gh pr create`:

```text
## Summary
## Changes
## Linked Issues
## Test Plan
## Benchmarks
## Evidence (AC Mapping)
## Checklist
```

PASS: PR-Body wird mit allen Gate-Sektionen erstellt.

### AC-3 - Close-Sequenz bleibt korrekt

Geplante Reihenfolge:

```bash
git push -u origin feat/issue-314-agent-model-policy
gh pr create --repo silentspike/project-sentinel ...
# CI gruen, PR merge
gh issue edit 314 --repo silentspike/project-sentinel --add-label "status:verified" --remove-label "status:in-progress"
gh issue close 314 --repo silentspike/project-sentinel
```

PASS: `status:verified` wird erst nach finaler Verifikation/Merge gesetzt und vor dem Issue-Close.

## Task 9 - Plan-Verifikation

### AC-1 - Plan-Slices abgeschlossen

Command:

```bash
git log --oneline --decorate -10
```

Output excerpt:

```text
645553b Task [8]: Dokumentation und PR-Vorbereitung
00f10ec Task [7]: Phase 8 - AC-Matrix und Live-Verifikation
9495871 Task [6]: Phase 6 - Gateway Deploy auf VM
9d2adf5 Task [5]: Phase 5 - Benchmarks
b326a2f Task [4]: Phase 4 - Go-Tests
114732d Task [3]: Phase 3 - Observability und Response Log
b953e4a Task [2]: Phase 2 - Gateway Policy-Layer
a42ef76 Task [1]: Phase 1 - Issue-Body-Repair, Branch und Preflight
```

PASS: Plan-Slices 1 bis 8 sind committed; Task 9 ist diese finale Verifikation.

### AC-2 - Tests und Build final gruen

Command:

```bash
go test ./cmd/cortex-gateway/internal/proxy ./cmd/cortex-gateway/internal/control
go build ./cmd/cortex-gateway
```

Output:

```text
ok  	github.com/obtFusi/project-sentinel/cmd/cortex-gateway/internal/proxy	(cached)
ok  	github.com/obtFusi/project-sentinel/cmd/cortex-gateway/internal/control	(cached)
```

`go build ./cmd/cortex-gateway` exited `0`.

PASS: Betroffene Gateway-Packages testen und bauen final.

### AC-3 - VM-Deploy bleibt live und gesund

Command:

```bash
ssh ubuntu@10.0.0.240 "curl -s localhost:8080/health; printf '\n--- traffic ---\n'; curl -s localhost:8081/control/traffic-stats | python3 -c 'import sys,json; d=json.load(sys.stdin); print({k:d.get(k) for k in [\"agent_runtime_model_policy\",\"last_agent_runtime_effective_model\",\"last_agent_runtime_policy_source\",\"last_agent_runtime_provider\",\"primary_provider\",\"external_mitm_provider\"]})'"
```

Output:

```text
{"status":"ok","version":"0.1.0","circuit_breakers":{"anthropic-direct":"closed","claude-code":"closed"},"guardrails_enabled":false}

--- traffic ---
{'agent_runtime_model_policy': 'haiku', 'last_agent_runtime_effective_model': 'haiku', 'last_agent_runtime_policy_source': 'agent_runtime_policy', 'last_agent_runtime_provider': 'claude-code', 'primary_provider': 'claude-code', 'external_mitm_provider': 'anthropic-direct'}
```

PASS: Der deployed Gateway zeigt die #314-Policy weiterhin live.

### AC-4 - GitHub Issue bleibt korrekt offen bis PR/Merge

Command:

```bash
gh issue view 314 --repo silentspike/project-sentinel --json number,state,labels,title,updatedAt
```

Output excerpt:

```json
{
  "number": 314,
  "state": "OPEN",
  "title": "Policy: Agent LLM model defaults via Gateway/Inference layer",
  "labels": ["quality:ready", "status:in-progress", "type:feature", "comp:cortex", "comp:inference"]
}
```

PASS: Issue ist nicht verfrueht geschlossen; `status:verified` kommt erst nach PR/Merge.

### AC-5 - Finale Sequenz

Nach Task-9-Commit:

```bash
git push -u origin feat/issue-314-agent-model-policy
gh pr create --repo silentspike/project-sentinel ...
# CI gruen abwarten
gh pr merge ...
gh issue edit 314 --repo silentspike/project-sentinel --add-label "status:verified" --remove-label "status:in-progress"
gh issue close 314 --repo silentspike/project-sentinel
```

PASS: Lokaler Stand ist bereit fuer die GitHub-Sequenz.
