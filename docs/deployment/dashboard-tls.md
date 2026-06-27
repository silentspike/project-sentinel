# Dashboard TLS Deployment Modes

Project Sentinel supports two dashboard TLS modes for `sentinel-dashboard-backend`.
The mode is selected entirely by environment variables.

## Mode Summary

| Mode | TLS env | Certificate source | `/api/cert-hash` | WebTransport behavior | Operator browser result |
| --- | --- | --- | --- | --- | --- |
| Zero-Config | unset | generated 13-day self-signed cert | `hash` is a base64 SHA-256 value | `serverCertificateHashes` pinning | first page load shows the usual self-signed warning |
| Production | `SENTINEL_DASHBOARD_TLS_CERT` and `SENTINEL_DASHBOARD_TLS_KEY` set | deployer-provided PEM files | `hash` is `null` | normal CA validation, no pinning | no certificate warning when the issuing CA is trusted |

Zero-Config remains the default. Do not set any TLS env variables when you want
the current self-signed behavior.

Production mode is for a hostname and a certificate chain that the operator
browser trusts. The backend does not obtain or renew certificates; it only loads
the files you provide.

## Environment Variables

| Variable | Required | Meaning |
| --- | --- | --- |
| `SENTINEL_DASHBOARD_TLS_CERT` | Production only | PEM certificate path. Use a full chain when the CA issues intermediates. |
| `SENTINEL_DASHBOARD_TLS_KEY` | Production only | PEM private-key path for the certificate. |
| `SENTINEL_DASHBOARD_HOSTNAME` | Optional | Human-readable deployment hostname for logs and operator docs. The browser still validates the SANs in the certificate. |

`SENTINEL_DASHBOARD_TLS_CERT` and `SENTINEL_DASHBOARD_TLS_KEY` must be set
together. If only one is set, startup fails instead of silently falling back to
the self-signed mode.

The 13-day validity rule applies only to Zero-Config hash-pinning mode. A
provided Production certificate keeps its own validity period because
`serverCertificateHashes` is not used.

## Zero-Config Mode

Use this mode for local and single-VM operation where immediate startup matters
more than a clean browser trust chain.

```bash
sudo systemctl unset-environment SENTINEL_DASHBOARD_TLS_CERT
sudo systemctl unset-environment SENTINEL_DASHBOARD_TLS_KEY
sudo systemctl restart sentinel-dashboard-backend
```

Expected checks:

```bash
curl -sk https://127.0.0.1:8001/api/cert-hash
# {"algorithm":"sha-256","hash":"<base64-sha256>"}
```

The browser may show `ERR_CERT_AUTHORITY_INVALID` on the first HTTPS page load.
After the operator accepts the warning, the console uses
`serverCertificateHashes` for WebTransport.

## Production Mode With An Internal CA

This example creates a test root CA and a server certificate for
`sentinel.lab`. For real deployments, use your organization CA process and keep
private keys outside the repository.

Create a root and server certificate:

```bash
openssl genrsa -out root.key 4096
openssl req -x509 -new -nodes -key root.key -sha256 -days 3650 \
  -subj "/CN=Project Sentinel Dashboard Root" -out root.pem

openssl genrsa -out dashboard.key 2048
openssl req -new -key dashboard.key -subj "/CN=sentinel.lab" -out dashboard.csr

cat > dashboard.ext <<'EOF'
basicConstraints = CA:FALSE
keyUsage = critical,digitalSignature,keyEncipherment
extendedKeyUsage = serverAuth
subjectAltName = DNS:sentinel.lab,IP:10.0.0.240
EOF

openssl x509 -req -in dashboard.csr -CA root.pem -CAkey root.key \
  -CAcreateserial -out dashboard.pem -days 397 -sha256 -extfile dashboard.ext
```

Install the server files on the deploy host. The service account must be able to
read both files.

```bash
sudo install -d -m 750 /opt/sentinel/tls
sudo install -m 640 dashboard.pem /opt/sentinel/tls/dashboard.pem
sudo install -m 640 dashboard.key /opt/sentinel/tls/dashboard.key
```

Set the dashboard env file used by the service:

```bash
sudo install -d -m 750 /opt/sentinel/config
sudo install -m 600 /dev/null /opt/sentinel/config/dashboard-backend.env
sudoedit /opt/sentinel/config/dashboard-backend.env
```

Example env entries:

```text
SENTINEL_DASHBOARD_HOSTNAME=sentinel.lab
SENTINEL_DASHBOARD_TLS_CERT=/opt/sentinel/tls/dashboard.pem
SENTINEL_DASHBOARD_TLS_KEY=/opt/sentinel/tls/dashboard.key
```

Import `root.pem` into each operator device trust store. On Ubuntu system trust:

```bash
sudo install -m 644 root.pem /usr/local/share/ca-certificates/sentinel-dashboard-root.crt
sudo update-ca-certificates
```

For Chromium profiles that use NSS, import the root into the profile database:

```bash
mkdir -p "$HOME/.pki/nssdb"
certutil -N -d "sql:$HOME/.pki/nssdb" --empty-password
certutil -A -d "sql:$HOME/.pki/nssdb" -n sentinel-dashboard-root \
  -t "C,," -i root.pem
certutil -L -d "sql:$HOME/.pki/nssdb" -n sentinel-dashboard-root
```

Do not use `--ignore-certificate-errors`, `ignoreHTTPSErrors`, or equivalent
bypass flags as Production evidence. The browser must trust the issuing root for
real.

Restart and verify:

```bash
sudo systemctl restart sentinel-dashboard-backend

curl --cacert root.pem --resolve sentinel.lab:8001:10.0.0.240 \
  https://sentinel.lab:8001/api/health
# {"service":"sentinel-dashboard-backend","status":"ok"}

curl --cacert root.pem --resolve sentinel.lab:8001:10.0.0.240 \
  https://sentinel.lab:8001/api/cert-hash
# {"algorithm":"sha-256","hash":null}
```

When the browser trusts the CA and connects to a hostname or IP covered by the
certificate SAN, the console should load without `ERR_CERT_AUTHORITY_INVALID`.
WebTransport should connect without `serverCertificateHashes`.

## Production Mode With Let's Encrypt

Use Let's Encrypt for deployments with a real DNS name reachable by the ACME
challenge method you choose. The backend does not run an ACME client.

Example using an external certbot flow:

```bash
sudo certbot certonly --standalone -d sentinel.example.com
```

Configure the backend:

```text
SENTINEL_DASHBOARD_HOSTNAME=sentinel.example.com
SENTINEL_DASHBOARD_TLS_CERT=/etc/letsencrypt/live/sentinel.example.com/fullchain.pem
SENTINEL_DASHBOARD_TLS_KEY=/etc/letsencrypt/live/sentinel.example.com/privkey.pem
```

Restart the backend after each renewal, for example with a certbot deploy hook:

```bash
sudo install -d -m 755 /etc/letsencrypt/renewal-hooks/deploy
cat <<'EOF' | sudo tee /etc/letsencrypt/renewal-hooks/deploy/restart-sentinel-dashboard.sh
#!/bin/sh
systemctl restart sentinel-dashboard-backend
EOF
sudo chmod 755 /etc/letsencrypt/renewal-hooks/deploy/restart-sentinel-dashboard.sh
```

If the private key is not readable by the backend service account, adjust file
ownership or group membership according to your host policy.

## Exposure Boundary

TLS mode and network exposure are separate controls. Issue #474 defines the
dashboard bind and exposure policy: loopback is the default, SSH TCP tunnels are
HTTP-only for this service because WebTransport uses QUIC/UDP, and deliberate
remote exposure is an explicit operator configuration decision.

Use `docs/security/dashboard-key-management.md` for the #474 operator-key,
rate-limit, bind-address, and tunnel guidance. This TLS guide does not change
authentication, bind addresses, firewall rules, or gateway/judge behavior.
