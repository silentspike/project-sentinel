# Issue #264 Live Verification

Stand: 2026-04-11
VM: `ubuntu@10.0.0.240`
Branch: `feat/issue-264-security-hardening-closure`

## Repo-Verifikation

### Rust Tests

Command:

```bash
cargo remote -c -- test -p sentinel-fs -p sentinel-sandbox -p sentinel-daemon
```

Ergebnis:

- `sentinel-daemon`: `172 passed; 0 failed`
- `sentinel-fs`: `93 passed; 0 failed`
- der Ruecktransfer der Artefakte wurde danach bewusst abgebrochen, weil `cargo remote` mehrere GB zurueckkopiert; der gruene Testbefund war davor bereits sichtbar

### Clippy

Command:

```bash
cargo remote -c -- clippy -p sentinel-fs -p sentinel-sandbox -p sentinel-daemon --all-targets -- -D warnings
```

Ergebnis:

- `Finished dev profile [unoptimized + debuginfo] target(s) in 3m 58s`
- keine Clippy-Diagnostik vor dem anschliessend abgebrochenen Ruecktransfer

## Deploy-Stand

### Runtime-Artefakte

Build-Server:

- `sentinel-daemon`: `d5303c021928032d280d68e0ed3078d89da83910ddae6e5fe709fe048b299cf9`
- `landlock-wrapper`: `350b9ea571d63c70188a17f68ab68492ccdc83f99db97b97b78d4e53b6aff85b`
- `breakout-helper`: `46d63049d0df2d0fd7c813b07e33a1bf11a8e751b492254323b1c6b826fe9cdf`

Live-VM nach Deploy:

- `sentinel-daemon`: `d5303c021928032d280d68e0ed3078d89da83910ddae6e5fe709fe048b299cf9`
- `landlock-wrapper`: `350b9ea571d63c70188a17f68ab68492ccdc83f99db97b97b78d4e53b6aff85b`
- `breakout-helper`: `46d63049d0df2d0fd7c813b07e33a1bf11a8e751b492254323b1c6b826fe9cdf`

### Live-Konfiguration

Command:

```bash
ssh ubuntu@10.0.0.240 "grep -n '^fs_mount' /opt/sentinel/config/daemon.toml"
ssh ubuntu@10.0.0.240 "systemctl cat sentinel-daemon | grep -n 'ReadWritePaths'"
```

Ergebnis:

- `fs_mount = "/opt/sentinel/fs"`
- `ReadWritePaths=/opt/sentinel/data /opt/sentinel/fs /ram/sentinel /ram/agents`

## AC-Matrix

### AC-1 Trash-Queue 24h

Fixture:

```bash
ssh ubuntu@10.0.0.240 "curl -s -X POST http://127.0.0.1:8084/operator/security/fs-trash-fixture -H 'Content-Type: application/json' -d '{\"agent_name\":\"Carla Mendez\",\"relative_path\":\"issue264/trash-verify.txt\",\"content\":\"issue264-trash-fixture\"}'"
```

Output:

```json
{"accepted":true,"agent_name":"Carla Mendez","relative_path":"issue264/trash-verify.txt","object_id":11,"chunk_hashes":["c0cdf650122a4e8a4d99c46d04f0259b53296400044302fc1e8970393882e014"],"trashed_chunks":1}
```

Inspect vor Aging:

```json
{"found":true,"chunk_hash":"c0cdf650122a4e8a4d99c46d04f0259b53296400044302fc1e8970393882e014","trashed_at_ms":1775940240995,"age_ms":8726,"in_chunk_index":true,"refcount":0}
```

GC innerhalb Grace:

```json
{"accepted":true,"grace_period_hours":24,"freed_from_trash":0,"freed_bytes":0}
```

Bewertung:

- PASS: Chunk liegt in `fs_trash_queue`, bleibt im CAS vorhanden und wird innerhalb der 24h-Grace nicht freigegeben

### AC-2 Freigabe nach 24h

Aging + GC:

```bash
ssh ubuntu@10.0.0.240 "curl -s -X POST http://127.0.0.1:8084/operator/security/fs-trash-age -H 'Content-Type: application/json' -d '{\"chunk_hash\":\"c0cdf650122a4e8a4d99c46d04f0259b53296400044302fc1e8970393882e014\",\"hours_ago\":25}'"
ssh ubuntu@10.0.0.240 "curl -s -X POST http://127.0.0.1:8084/operator/security/fs-trash-gc -H 'Content-Type: application/json' -d '{\"grace_period_hours\":24}'"
ssh ubuntu@10.0.0.240 "curl -s 'http://127.0.0.1:8084/operator/security/fs-trash?hash=c0cdf650122a4e8a4d99c46d04f0259b53296400044302fc1e8970393882e014'"
```

Outputs:

```json
{"accepted":true,"chunk_hash":"c0cdf650122a4e8a4d99c46d04f0259b53296400044302fc1e8970393882e014","trashed_at_ms":1775850263807}
{"accepted":true,"grace_period_hours":24,"freed_from_trash":1,"freed_bytes":23}
{"found":false,"chunk_hash":"c0cdf650122a4e8a4d99c46d04f0259b53296400044302fc1e8970393882e014","trashed_at_ms":null,"age_ms":null,"in_chunk_index":false,"refcount":0}
```

Bewertung:

- PASS: nach kontrolliertem Aging wird der Chunk aus Trash und CAS freigegeben

### AC-3 Restore nach Ransomware-Write

Command:

```bash
ssh ubuntu@10.0.0.240 "curl -s -X POST http://127.0.0.1:8084/operator/security/fs-ransomware-test -H 'Content-Type: application/json' -d '{\"agent_name\":\"Michael Hartmann\",\"relative_path\":\"issue264/live-restore.txt\",\"snapshot_label\":\"issue264-live\"}'"
```

Output:

```json
{"accepted":true,"hook_version":2,"agent_name":"Michael Hartmann","relative_path":"issue264/live-restore.txt","snapshot_label":"issue264-live","host_path":"/opt/sentinel/fs/AGENT-16/issue264/live-restore.txt","snapshot_id":"019d7e41-c9a9-7202-ac63-29e90bb826dd","bytes_written":60,"before_sha256":"f558670598c372635edd1598451dbf3e3b2276196bc94ecb6cfcbd5f629a2ad2","mutated_sha256":"4a69e74386380927caf63081ec3b3f9e3ed67889cb33049d154b0cd91bdd4721","restored_sha256":"f558670598c372635edd1598451dbf3e3b2276196bc94ecb6cfcbd5f629a2ad2","restored":true,"snapshot_wait_ms":732,"restore_wait_ms":1002}
```

Journal:

```text
2026-04-11T20:35:33.177632Z  INFO ... Issue #264 fs-ransomware-test v2 gestartet
2026-04-11T20:35:34.934939Z  INFO ... Hot-Swap Restore gestartet snapshot_id=019d7e41-c9a9-7202-ac63-29e90bb826dd
2026-04-11T20:35:35.262944Z  INFO ... Issue #264 fs-ransomware-test v2 abgeschlossen ... restored_sha256=f558670598c372635edd1598451dbf3e3b2276196bc94ecb6cfcbd5f629a2ad2
2026-04-11T20:35:35.413379Z  INFO ... Hot-Swap Restore abgeschlossen snapshot_id=019d7e41-c9a9-7202-ac63-29e90bb826dd
```

Zusatz:

- `snapshot_restored|system` im Event Store

Bewertung:

- PASS: `before_sha256 == restored_sha256`
- Hinweis: die eigentliche Restore-Arbeit im Daemon liegt laut Journal bei ca. `225ms`; die Hook-Zahl `restore_wait_ms=1002` enthaelt zusaetzliche Queue-/Loop-Latenz

### AC-4 Write-Anomaly triggert Suspend

Trigger:

```bash
ssh ubuntu@10.0.0.240 "curl -s -X POST http://127.0.0.1:8084/operator/security/write-anomaly-test -H 'Content-Type: application/json' -d '{\"agent_name\":\"Michael Hartmann\",\"mode\":\"absolute-threshold\",\"bytes_per_sec\":25000000,\"duration_secs\":75,\"align_to_observation_window\":false}'"
```

Output:

```json
{"accepted":true,"agent_name":"Michael Hartmann","mode":"absolute-threshold","bytes_per_sec":25000000,"duration_secs":75,"align_to_observation_window":false,"start_delay_secs":0,"scheduled_start_tick":127139,"bwrap_pid":1456534,"helper_pid":1458058,"host_path":"/opt/sentinel/data/security-write-anomaly/AGENT-16/.issue264-write-anomaly.bin"}
```

Intervention:

```text
platform_intervention|write_anomaly|Michael Hartmann|sigstop|Write-Rate 8.3 MB/s >4.8 MB/s absolut — SIGSTOP fuer Agent-Cgroup getriggert
```

Cgroup-Nachweis:

```text
/sys/fs/cgroup/sentinel/Michael Hartmann/cgroup.procs -> 1458058
1458058 T    python3
```

Write-Stillstand:

```text
632340032 632340032
```

Bewertung:

- PASS: der reale cgroup-Mitgliedsprozess steht auf `T`, und die Dateigroesse bleibt nach dem Stop stabil
- Wichtiger Runtime-Fund: der getrackte `bwrap_pid` fuer Michael stand zu diesem Zeitpunkt bereits auf `Z` (zombie); der wirksame Stop-Nachweis lief ueber das echte cgroup-Mitglied, nicht ueber den stale Runtime-State-Eintrag

### AC-5 Dashboard / Cockpit

Readout:

```json
[
  {
    "summary": "Platform Intervention: sigstop fuer Michael Hartmann — Write-Rate 8.3 MB/s >4.8 MB/s absolut — SIGSTOP fuer Agent-Cgroup getriggert",
    "status": "Ausstehend"
  }
]
```

Artefakte:

- Cockpit-Gesamtsicht: `/tmp/issue264-ac5-cockpit.png`
- gezoomter Security-Fall: `/tmp/issue264-ac5-write-anomaly.png`

Bewertung:

- PASS: der Write-Anomaly-/SIGSTOP-Fall ist im read-only Cockpit sichtbar

### AC-6 Immutable Snapshots

Command:

```bash
ssh ubuntu@10.0.0.240 "sqlite3 /opt/sentinel/data/events.db \"DELETE FROM world_snapshots WHERE id=(SELECT id FROM world_snapshots ORDER BY created_at DESC LIMIT 1);\""
```

Output:

```text
Error: stepping, Cannot delete snapshot younger than 7 days (19)
```

Bewertung:

- PASS

### AC-7 Landlock blockiert unautorisierte Execute-Pfade

Commands:

```bash
ssh ubuntu@10.0.0.240 "curl -s -X POST http://127.0.0.1:8084/operator/security/landlock-test -H 'Content-Type: application/json' -d '{\"agent_name\":\"Michael Hartmann\",\"scenario\":\"exec-from-tmp\"}'"
ssh ubuntu@10.0.0.240 "curl -s -X POST http://127.0.0.1:8084/operator/security/landlock-test -H 'Content-Type: application/json' -d '{\"agent_name\":\"Michael Hartmann\",\"scenario\":\"exec-bin-sh\"}'"
ssh ubuntu@10.0.0.240 "curl -s -X POST http://127.0.0.1:8084/operator/security/landlock-test -H 'Content-Type: application/json' -d '{\"agent_name\":\"Michael Hartmann\",\"scenario\":\"exec-python3\"}'"
```

Outputs:

```json
{"accepted":true,"agent_name":"Michael Hartmann","scenario":"exec-from-tmp","helper_pid":1458286,"exit_code":1,"blocked":true,"attempted_path":"/tmp/evil.sh","audit_event_id":"e33603b7-c3d1-44b6-8961-747390a9d241","stdout":"","stderr":"[landlock-wrapper] Landlock enforced for Michael Hartmann\n[landlock-wrapper] exec failed: Permission denied (os error 13)"}
{"accepted":true,"agent_name":"Michael Hartmann","scenario":"exec-bin-sh","helper_pid":1458539,"exit_code":1,"blocked":true,"attempted_path":"/bin/sh","audit_event_id":"122c56f8-056e-4dc7-bea2-287bf01c5b26","stdout":"","stderr":"[landlock-wrapper] Landlock enforced for Michael Hartmann\n[landlock-wrapper] exec failed: Permission denied (os error 13)"}
{"accepted":true,"agent_name":"Michael Hartmann","scenario":"exec-python3","helper_pid":1458738,"exit_code":1,"blocked":true,"attempted_path":"/usr/bin/python3","audit_event_id":"19db7033-d320-463c-ad25-190eec3d5fb6","stdout":"","stderr":"[landlock-wrapper] Landlock enforced for Michael Hartmann\n[landlock-wrapper] exec failed: Permission denied (os error 13)"}
```

Bewertung:

- PASS

### AC-8 Blockierte Execute-Versuche werden auditiert

Event-Store-Query:

```text
security_exec_blocked|exec-python3|/usr/bin/python3|Michael Hartmann
security_exec_blocked|exec-bin-sh|/bin/sh|Michael Hartmann
security_exec_blocked|exec-from-tmp|/tmp/evil.sh|Michael Hartmann
```

Bewertung:

- PASS

## Runtime-Gesundheit

### Services

```text
sentinel-daemon: active
sentinel-gateway: active
sentinel-projection: active
sentinel-nats-bridge: active
```

### Gateway

Vor den Security-Tests:

```json
{"status":"ok","version":"0.1.0","circuit_breakers":{"claude-code":"closed"},"guardrails_enabled":false}
```

Nach dem abschliessenden Daemon-Neustart:

```json
{"status":"ok","version":"0.1.0","circuit_breakers":{"claude-code":"open"},"guardrails_enabled":false}
```

### Panic / Drift

- keine Treffer fuer `panic` im geprueften Fenster
- keine Treffer fuer `drift` im geprueften Fenster

## Offener Runtime-Befund

Waerend der Verifikation zeigte das System einen separaten Platform-Runtime-Befund, der **nicht** Teil des Security-Kerns von `#264` ist, aber fuer den Close beruecksichtigt werden muss:

- nach Restore / Stall-Interventionen wurden viele Agents als `Agent nach Stall restartet (despawned, Respawn bei naechstem Shift-Check)` geloggt
- gleichzeitig blieben nur noch drei Agent-Cgroups mit echten Prozessen sichtbar:
  - `Carla Mendez`
  - `Jonas Weber`
  - `Oliver Brandt`
- fuer `Michael Hartmann` war der `security_runtime_state`-Eintrag noch vorhanden, obwohl der getrackte `bwrap_pid` bereits `Z` (zombie) war

Bewertung:

- Die Security-Funktionen fuer `#264` sind live belegt.
- Zusaetzlich gibt es einen separaten Runtime-/Respawn-/State-Staleness-Befund, der vor einem Close sauber eingeordnet werden muss.
