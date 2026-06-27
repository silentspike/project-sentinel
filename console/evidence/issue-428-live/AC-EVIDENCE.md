# #428 Agent Deep View — Live-Verifikation (Deploy-VM 10.0.0.240) — Evidence

Deploy: Release-Binaries `sentinel-daemon` + `sentinel-dashboard-backend` nach `/opt/sentinel/bin/`
(Pfad via `grep ExecStart` verifiziert), Console-Bundle nach `/opt/sentinel/console-dist`; daemon +
dashboard-backend restartet. **gateway/judge bleiben inactive** (`/etc/sentinel/allow-llm` absent →
kein LLM/Token-Risiko). journalctl nach Restart: 0 Panics/FATAL, „Sandbox Enforcer init warnings=0".
Test-Agent: `agent_id=8` = „Lena Hoffmann" = AGENT-08.

## AC-1 — FS-Browser listet/liest Agent-Dateien read-only (Dirents + Content + Dedup/Size)

Daemon-Operator-API (loopback :8084) + Dashboard-Proxy (HTTPS :8001, Session-Cookie):
```
# Root nach Populate (fs-dedup-benchmark schrieb echte Dateien via write_file):
agent-fs?agent_id=8&inode=1  -> entries: [{name:".issue379-dedup", kind:"dir", inode:2}]
                                dedup_ratio_percent: 94.75, cas_blob_count: 63
# Verzeichnis-Listing (inode 2): 5 Dateien je 256 B, refcount 5 (inhaltsgleich -> 1 CAS-Blob)
agent-fs?agent_id=8&inode=2   -> 5x {kind:"file", size:256, refcount:5}
# File-Read (read-only, size-cap 1 MiB):
agent-fs-read?agent_id=8&inode=4 -> {accepted:true, size:256, returned_bytes:256,
                                     truncated:false, encoding:"hex", refcount:5}
```
**Auth-Negativ (Auflage E):** Dashboard-FS-Browse-GET **ohne Session-Cookie** -> `no_session_http=401`
(`require_auth`); mit Session -> 200 (5 entries, refcount 5, dedup 94.75%). Daemon-Operator-API hat auf
dieser VM keinen `shared_secret` (loopback-only by design); die Operator-Key-401-Grenze ist per Unit-Test
belegt (`agent_fs_browse_requires_operator_key`). **Read-only:** kein `write_file`/`mkdir` auf dem Pfad
(Code-Review + Route-Liste); die einzigen Writes waren das bewusste Populate. PASS

## AC-2 — per-Agent Sparklines + Tool-Donut aus echten Event/eBPF-Daten (kein Mock)

`/api/events?agent=AGENT-08&limit=200` -> 40 echte Events (bio_state_updated 32, agent_status_changed 4,
agent_spawned 1, agent_despawned 1, resource_profile_changed 2). playwright gegen `https://localhost:8001`
(SSH-Tunnel, ignoreHTTPSErrors; Deep-View via `#deep=8` Deep-Link — die WT-gepushte Agent-Liste tunnelt
als QUIC/UDP nicht, der AgentsView-Klick-Entry ist per vitest belegt):
```
OPEN_MS=92   SPARK_POINTS_LEN=251   DONUT_LEGEND=5 DONUT_CIRCLES=5
FS_DEDUP="Dedup: 94.7% gespart (64 CAS-Blobs, 3570960 B)"
```
`pw-agent-deep.png` (visuell gesichtet): die AgentDeepView zeigt nicht-leere Sparkline + farbigen
Tool-Donut + Event-Typ-Legende + „Dateisystem (read-only, CAS-FUSE)"-Sektion. Werte korrelieren mit
`/api/events`. PASS

## AC-3 — per-Agent Start/Stop wirkt durch den Daemon (Status nachweislich, NICHT despawned)

projection.db `agent_live_view` (Ground-Truth) + Proc-State des bwrap-PID:
```
BEFORE:     status=active     present=1  proc=S
STOP:  -> {new_status:"suspended", affected_pids:1, outcome:"ok", note:"paused (SIGSTOP; ECS + Memory bleiben)"}
AFTER STOP: status=suspended  present=1  proc=T   # NICHT despawned: ECS-Entity + Memory bleiben; Prozess eingefroren
START: -> {new_status:"active", outcome:"ok"}
AFTER START: status=active    present=1  proc=S   # gleiche PID -> resumed, nicht respawnt
```
End-to-End ueber den **Dashboard-Proxy** (console-Pfad): `POST /api/control/agent/8/stop` -> suspended,
`/start` -> active. **Ehrliche Abweichung (Control-Entscheid):** der kanonische Status-String ist
`"suspended"` (AgentStatus-Variante); die Console rendert ihn als „Pausiert"/„Paused". Literal-`"paused"`
bewusst weggelassen (sonst Enum-Duplikat / zweite Wahrheitsquelle). PASS

### AC-3b — Restart-Konsistenz (Auflage B)
```
pause 8 -> status=suspended proc=T ; restart daemon ; AFTER RESTART: newpid=240768 proc=T status=active
```
**Load-bearing erfuellt:** der pausierte Agent wird nach Daemon-Restart neu gespawnt und **sofort
re-SIGSTOPpt** -> Proc-State `T` (Prozess eingefroren, laeuft NICHT „active" weiter). **Dokumentierte
Grenze:** das Projektions-/UI-Status-Label re-seedet beim Restart aus dem World-Snapshot auf `"active"`
(die ECS-Welt kennt kein Pause-Konzept; der Pause-Zustand lebt im runtime_orch-Handle) und
re-synchronisiert beim naechsten Pause/Resume. Eine Korrektur wuerde den hochsensiblen Restore-/Seed-Pfad
(#491) beruehren und ist bewusst nicht Teil dieses PRs. PASS (mit dokumentierter Grenze)

### AC-3c — Destructive-Remove eines PAUSIerten Agents (Hinweis C)
```
pause 8 -> suspended (proc T) ; remove -> {new_status:"despawned", note:"despawned (teardown_agent_full)"}
AFTER REMOVE: status=despawned  runtime_found=false  last_event=agent_despawned
reconcile respawn_missing -> respawned_agents=1 -> status=active runtime_found=true
```
`teardown_agent_full` raeumt den eingefrorenen (T-state) Prozess ab (SIGTERM-pending -> SIGKILL),
emittiert `AgentDespawned`, entfernt den Agent aus Runtime + Projection — klar abgegrenzt von Pause. PASS

## AC-4 — Tests grün
`cargo remote -c -- test -p sentinel-daemon` -> 309 passed; `-p sentinel-dashboard-backend` -> 42 passed;
`clippy --all-targets -D warnings` -> 0 warnings; `bunx vitest run` -> 63 passed / 14 Files; tsc + build exit 0. PASS

## Benchmarks (Deploy-VM, daemon-direct loopback; Details `docs/benchmarks/BENCHMARK-RESULTS.md`)
Leichte Latenz-Probe der deployten Funktion (kein Last-Test). System unperturbiert:
mem_used 859->863 MB / 7938, loadavg 0.18, CPU ~89% idle (mpstat).
```
FS-Browse Listing (root):  p50 2697 us  p95 3197 us   (n=100, inkl. HTTP+redb-Read)
FS-Browse Listing (dir):   p50 2814 us  p95 3782 us
FS-Read File:              p50  494 us  p95  606 us
Pause-Command round-trip:  p50 ~1000 ms (tick-synchron: Commands werden 1x/Tick gedrained @1Hz; SIGSTOP selbst µs)
Resume-Command round-trip: p50 ~1000 ms (dito)
Dedup (1:n): ratio 94.74 %, 65 CAS-Blobs, 3.57 MB gespart  (FS-Browse = inode/Pointer-Read, kein Datentransfer)
```

## Deploy-Zustand
Nur Sentinel-Binaries + Console-Bundle ersetzt (Backups `.bak-428*`); daemon/dashboard-backend/projection
active; gateway/judge inactive (kein Token). Test-Agent 8 am Ende `active` (resume/respawn-Cleanup).
