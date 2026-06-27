# Issue #442 Live Claude Client Availability Gap

Issue #442 AC-2 and the real-LLM portion of AC-3 require a minimal live `claude -p` run on the test VMs. The #442 code paths are implemented and covered with fake-Claude tests plus live deterministic generator evidence, but the test VMs currently do not have a Claude Code client or Node/npm installed.

No commands in this check touched `.240`.

## 10.0.0.241

Command:

```bash
ssh -o BatchMode=yes -o ConnectTimeout=8 ubuntu@10.0.0.241 'set -eu; echo host=$(hostname); echo claude=$(command -v claude || true); echo node=$(command -v node || true); echo npm=$(command -v npm || true); pgrep -af "(^|/)claude( |$)" || true'
```

Output:

```text
host=sentinel-test-node-0
claude=
node=
npm=
```

## 10.0.0.242

Command:

```bash
ssh -o BatchMode=yes -o ConnectTimeout=8 ubuntu@10.0.0.242 'set -eu; echo host=$(hostname); echo claude=$(command -v claude || true); echo node=$(command -v node || true); echo npm=$(command -v npm || true); pgrep -af "(^|/)claude( |$)" || true'
```

Output:

```text
host=sentinel-test-node-1
claude=
node=
npm=
```

## Decision Needed

To finish AC-2 and the real-LLM portion of AC-3 without weakening the evidence, Ops/ORC must decide whether an authenticated Claude Code client may be installed or copied onto `.241/.242`. Without that decision, the remaining live evidence can only prove fake-Claude session plumbing and deterministic setup generation, not a real `claude -p` execution or token cost.
