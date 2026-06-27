# Issue 473 Live Verification

Date: 2026-06-27

## Scope

- Repository branch: `feat/issue-473-tls-strategy`
- Deploy VM: `ubuntu@10.0.0.240`
- Build host: `root@10.0.0.155`
- Runtime service: `sentinel-dashboard-backend`
- Gateway/Judge remained inactive during live verification.

## Build And Test

```bash
cargo remote -H root@10.0.0.155 -t /tmp/builds -c -- build -p sentinel-dashboard-backend
```

```text
Finished `dev` profile [unoptimized + debuginfo] target(s) in 7.31s
```

```bash
cargo remote -H root@10.0.0.155 -t /tmp/builds -c -- test -p sentinel-dashboard-backend
```

```text
src/lib.rs: 48 passed
tests/auth_routes.rs: 7 passed
tests/config_routes.rs: 13 passed
tests/login_rate_limit.rs: 5 passed
tests/resilience.rs: 1 passed
tests/wt_roundtrip.rs: 4 passed
Doc-tests sentinel_dashboard_backend: 0 passed
```

```bash
cargo remote -H root@10.0.0.155 -t /tmp/builds -c -- build -p sentinel-dashboard-backend --release
```

```text
Finished `release` profile [optimized] target(s) in 5m 13s
```

## Deploy

```bash
ssh ubuntu@10.0.0.240 "systemctl cat sentinel-dashboard-backend | grep -E 'ExecStart|EnvironmentFile'"
```

```text
EnvironmentFile=-/opt/sentinel/config/dashboard-backend.env
ExecStart=/opt/sentinel/bin/sentinel-dashboard-backend
```

```text
new release sha256: d2d3f47821239f708afd8279e4f8d994d74f952d35695bbadad7f4fd56114627
pre-deploy sha256: d13863d714c029fe115641aff7584affe99fc91fddaff4be2246fa3df631085b
installed sha256: d2d3f47821239f708afd8279e4f8d994d74f952d35695bbadad7f4fd56114627
```

```bash
ssh ubuntu@10.0.0.240 "systemctl is-active sentinel-dashboard-backend"
```

```text
active
```

## AC-1 Zero-Config

Command:

```bash
xvfb-run -a env MODE=zero-config BASE_URL=https://127.0.0.1:8001 \
  OUT_DIR=/tmp/issue-473-live HEADLESS=false \
  BROWSER_EXECUTABLE=/home/ubuntu/.cache/ms-playwright/chromium-1148/chrome-linux/chrome \
  node /tmp/issue-473-live/playwright-dashboard-tls.js
```

Output:

```text
ignore_https_errors=false
certificate_bypass_requested=false
warning_detected=true
warning_goto_error=page.goto: net::ERR_CERT_AUTHORITY_INVALID at https://127.0.0.1:8001/
live_indicator="connected"
cert_hash_json={"algorithm":"sha-256","hash":"IkypcOID1l04fZQUNupcbdHSLRllT/NUZLm6jSYiRyM="}
cert_hash_present=true
```

Visual evidence:

- `zero-config-warning.svg`: Chromium warning with `NET::ERR_CERT_AUTHORITY_INVALID`.
- `zero-config-connected.svg`: dashboard connected after dismissing the expected warning.

## AC-2 Production TLS

CA setup:

```bash
bash /tmp/issue-473-live/setup-production-ca.sh
```

Output:

```text
nss_db=sql:/tmp/issue-473-live/chromium-ca-home/.pki/nssdb
Certificate Trust Flags:
    SSL Flags:
        Valid CA
        Trusted CA
system_root=/usr/local/share/ca-certificates/sentinel-issue473-root.crt
/tmp/issue-473-live/production-ca/server.pem: OK
```

Backend checks:

```bash
curl -sS --cacert /tmp/issue-473-live/production-ca/root.pem \
  -w '\nhttp_code=%{http_code}\n' https://127.0.0.1:8001/api/health
```

```text
{"service":"sentinel-dashboard-backend","status":"ok"}
http_code=200
```

```bash
curl -sS --cacert /tmp/issue-473-live/production-ca/root.pem \
  -w '\nhttp_code=%{http_code}\n' https://127.0.0.1:8001/api/cert-hash
```

```text
{"algorithm":"sha-256","hash":null}
http_code=200
```

Browser command:

```bash
xvfb-run -a env PLAYWRIGHT_MODULE=/home/ubuntu/pw-473/node_modules/playwright \
  MODE=production BASE_URL=https://localhost:8001 \
  OUT_DIR=/tmp/issue-473-live PROFILE_HOME=/tmp/issue-473-live/chromium-ca-home \
  HEADLESS=false BROWSER_EXECUTABLE=/home/ubuntu/.cache/ms-playwright/chromium-1228/chrome-linux64/chrome \
  EXTRA_BROWSER_ARGS='--use-system-ca --webtransport-developer-mode' \
  HOST_RESOLVER_RULES='MAP localhost 127.0.0.1' \
  node /tmp/issue-473-live/playwright-dashboard-tls.js
```

Output:

```text
ignore_https_errors=false
requested_browser_args=["--no-sandbox","--use-system-ca","--webtransport-developer-mode","--host-resolver-rules=MAP localhost 127.0.0.1"]
certificate_bypass_requested=false
warning_detected=false
live_indicator="connected"
cert_hash_json={"algorithm":"sha-256","hash":null}
cert_hash_present=false
```

Notes:

- `--ignore-certificate-errors` and `ignoreHTTPSErrors` were not used in Production.
- Chromium WebTransport Developer Mode was required for the local test CA because WebTransport rejects non-known roots even after local trust import.
- The root CA was still imported and verified through NSS and Ubuntu system trust.

Visual evidence:

- `production-connected.svg`: dashboard loads directly with no browser warning and WebTransport connected.

## Benchmarks

Raw JSON:

- `zero-config-benchmark.json`
- `production-benchmark.json`

Summary:

| Mode | TLS p50 | TLS p95 | WT p50 | WT p95 |
| --- | ---: | ---: | ---: | ---: |
| Zero-Config | 6.30 ms | 7.43 ms | 9.00 ms | 10.20 ms |
| Production | 5.65 ms | 6.58 ms | 10.50 ms | 12.20 ms |

## Final VM State

The service was restored to Zero-Config after AC-2 and benchmarks.

```bash
ssh ubuntu@10.0.0.240 "curl -sk https://127.0.0.1:8001/api/cert-hash"
```

```text
{"algorithm":"sha-256","hash":"8ySeHv+5i1m/f+dFtsP+29BS252oeofJotR6kZ6mV2A="}
```

```bash
ssh ubuntu@10.0.0.240 "systemctl is-active cortex-gateway"
ssh ubuntu@10.0.0.240 "systemctl is-active sentinel-judge"
```

```text
inactive
inactive
```
