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
