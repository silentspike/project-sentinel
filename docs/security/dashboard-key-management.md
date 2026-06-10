# Dashboard operator-key management (#474)

Operational runbook for the `sentinel-dashboard-backend` operator key and the login
brute-force protection. The operator key is the single control-plane credential: it gates the
console login, all `/api/control/*` mutations (chaos, pause/resume, provider switch,
snapshot/restore, config apply), the projection read routes, and the WebTransport ticket. Treat
it as a production secret.

Threat reference: `docs/security/threat-model.md`, Attacker Class 2 (external attacker). TOGAF
cluster 03 (infrastructure) / 07 (deployment).

## Key generation

Generate a high-entropy, deploy-specific secret **on the deploy host**, never a `smoke*`/test
value and never committed to the repository:

```bash
openssl rand -base64 32          # ~256 bits of entropy
```

Write it into the backend env-file:

```
# /opt/sentinel/config/dashboard-backend.env
SENTINEL_DASHBOARD_API_KEY=<generated-secret>
```

If the key is empty or unset the backend is **fail-closed**: `POST /api/auth/login` returns
`403` and no session can be minted.

## Storage and permissions

The env-file holds the secret and must be readable only by root (the service runs the unit's
`EnvironmentFile=-...` with a leading `-`, i.e. the file is optional — verify it exists with the
right mode):

```bash
stat -c '%a %U:%G' /opt/sentinel/config/dashboard-backend.env
# expected: 600 root:root
```

Never echo the key into logs, screenshots, benchmark artifacts, or commits.

## Rotation

1. Generate a new secret (`openssl rand -base64 32`).
2. Replace `SENTINEL_DASHBOARD_API_KEY` in `/opt/sentinel/config/dashboard-backend.env`.
3. Restart the service:

   ```bash
   sudo systemctl restart sentinel-dashboard-backend
   ```

A restart **discards the in-memory session store**, so every existing session is invalidated
immediately — rotation forces a complete operator re-authentication with the new key (this is
intentional; there is no stale-session window bounded by the 12 h TTL).

Verify after rotation (HTTPS is self-signed, so `-k`):

```bash
# old key -> 401
curl -sk -o /dev/null -w '%{http_code}\n' -X POST https://127.0.0.1:8001/api/auth/login \
  -H 'content-type: application/json' --data '{"key":"<OLD>"}'
# new key -> 200 + Set-Cookie
curl -sk -D - -o /dev/null -X POST https://127.0.0.1:8001/api/auth/login \
  -H 'content-type: application/json' --data '{"key":"<NEW>"}' | grep -i 'set-cookie\|HTTP/'
```

(Pass the key via a shell variable / `--data @-` from stdin rather than as a plain argument so it
does not appear in the process list on a multi-user host.)

## Login rate limit and audit log (#474)

Failed logins are rate-limited per client IP and audit-logged. The thresholds are
env-configurable on the service:

| Env var | Default | Meaning |
| --- | --- | --- |
| `SENTINEL_DASHBOARD_LOGIN_MAX_FAILS` | `5` | failed attempts within the window that engage a block |
| `SENTINEL_DASHBOARD_LOGIN_WINDOW_SECS` | `60` | rolling window length (seconds) |
| `SENTINEL_DASHBOARD_LOGIN_BLOCK_SECS` | `300` | block duration once the threshold is hit (seconds) |

Semantics: attempts `1..N` return `401`; from `N+1` on the IP gets `429` + `Retry-After`, checked
**before** the key comparison (so a blocked IP gets `429` even with the correct key — no key
oracle). A successful login resets the counter. Only `POST /api/auth/login` is limited.

Behind the loopback bind every client appears as `127.0.0.1`, so the per-IP limiter acts
effectively globally on the VM. That is accepted (anyone with VM access is already more
privileged); the env knobs are the tuning valve.

Audit entries (failed attempts, block engagement) are `tracing::warn!` lines — they never contain
the attempted key:

```bash
journalctl -u sentinel-dashboard-backend | grep 'audit:'
```

## Network exposure (#474 decision)

The backend binds **loopback-only** (`127.0.0.1:8001`, HTTP and WT/UDP) on the deploy VM, and the
UFW `:8001` rule is removed:

```bash
sudo ufw status numbered            # find the :8001 rule(s)
sudo ufw delete <n>                 # remove (check both tcp and any udp entry)
```

Access paths:

- **On-VM** (browser / playwright against `https://127.0.0.1:8001`): full console including the
  WebTransport live push.
- **`ssh -L 8001:127.0.0.1:8001 <user>@<deploy-vm>`**: HTTP/REST only — login and read routes
  work, but the WebTransport live push does not, because WebTransport is QUIC/**UDP** and an
  `ssh -L` tunnel forwards only TCP.
- **Full remote live console**: requires a UDP-capable overlay (WireGuard/ZTNA), tracked in #522
  (lab ops). This is distinct from the third-party product exposure model in #473.

Deliberate LAN/third-party exposure is an explicit opt-out: override
`SENTINEL_DASHBOARD_HTTP_BIND` / `SENTINEL_DASHBOARD_WT_BIND` via the env-file. The TLS/exposure
model for that case is defined in #473. Do not re-add a `0.0.0.0` bind to the unit.

## Verification checklist

```bash
systemctl cat sentinel-dashboard-backend | grep BIND        # 127.0.0.1:8001 (HTTP + WT)
ss -ltnup | grep 8001                                       # listens on 127.0.0.1, not 0.0.0.0
sudo ufw status | grep 8001 || echo 'no :8001 rule (expected)'
stat -c '%a %U:%G' /opt/sentinel/config/dashboard-backend.env  # 600 root:root
journalctl -u sentinel-dashboard-backend | grep 'audit:'    # audit entries present after attempts
```
