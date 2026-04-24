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

PASS: Betroffene Gateway-Packages testen und bauen nach Task 2.

### Zwischenfund

Der erste `go test`-Lauf zeigte, dass die Policy zu frueh vor dem Synthesis-Pfad lief und Testprovider-Forwards blockierte. Fix:

- Policy-Anwendung wurde hinter Synthesis/API-CP/Sequencing und direkt vor Streaming/Provider.Send verschoben.
- Testprovider `mock` wird als Testmapping fuer `haiku` akzeptiert; echte unbekannte Provider bleiben fail-closed.
