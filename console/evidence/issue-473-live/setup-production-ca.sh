#!/usr/bin/env bash
set -euo pipefail

WORK_DIR=${WORK_DIR:-/tmp/issue-473-live/production-ca}
NSS_HOME=${NSS_HOME:-/tmp/issue-473-live/chromium-ca-home}
ENV_FILE=${ENV_FILE:-/opt/sentinel/config/dashboard-backend.env}
CERT_DIR=${CERT_DIR:-/opt/sentinel/console-cert/issue-473-production}
ROOT_NICKNAME=${ROOT_NICKNAME:-sentinel-issue473-root}
SYSTEM_ROOT=${SYSTEM_ROOT:-/usr/local/share/ca-certificates/sentinel-issue473-root.crt}

ROOT_KEY="$WORK_DIR/root.key"
ROOT_CERT="$WORK_DIR/root.pem"
SERVER_KEY="$WORK_DIR/server.key"
SERVER_CSR="$WORK_DIR/server.csr"
SERVER_CERT="$WORK_DIR/server.pem"
SERVER_CHAIN="$WORK_DIR/server-chain.pem"
SERVER_CNF="$WORK_DIR/server.cnf"
INSTALLED_CERT="$CERT_DIR/server.pem"
INSTALLED_KEY="$CERT_DIR/server.key"
NSS_DB="$NSS_HOME/.pki/nssdb"
BACKUP_FILE="$ENV_FILE.issue473-pre-production"

echo "work_dir=$WORK_DIR"
echo "nss_home=$NSS_HOME"
echo "env_file=$ENV_FILE"
echo "cert_dir=$CERT_DIR"

mkdir -p "$WORK_DIR"
rm -rf "$NSS_HOME"
mkdir -p "$NSS_DB"

openssl genrsa -out "$ROOT_KEY" 4096
openssl req -x509 -new -nodes -key "$ROOT_KEY" -sha256 -days 3650 \
  -subj "/CN=Sentinel Issue 473 Test Root" \
  -out "$ROOT_CERT"

openssl genrsa -out "$SERVER_KEY" 2048
cat > "$SERVER_CNF" <<'CNF'
[req]
distinguished_name = dn
req_extensions = v3_req
prompt = no

[dn]
CN = 127.0.0.1

[v3_req]
subjectAltName = @alt_names
extendedKeyUsage = serverAuth
keyUsage = digitalSignature, keyEncipherment

[alt_names]
IP.1 = 127.0.0.1
DNS.1 = localhost
CNF

openssl req -new -key "$SERVER_KEY" -out "$SERVER_CSR" -config "$SERVER_CNF"
openssl x509 -req -in "$SERVER_CSR" -CA "$ROOT_CERT" -CAkey "$ROOT_KEY" -CAcreateserial \
  -out "$SERVER_CERT" -days 365 -sha256 -extensions v3_req -extfile "$SERVER_CNF"
cat "$SERVER_CERT" "$ROOT_CERT" > "$SERVER_CHAIN"

sudo install -d -m 755 -o root -g root "$CERT_DIR"
sudo install -m 644 -o root -g root "$SERVER_CHAIN" "$INSTALLED_CERT"
sudo install -m 600 -o root -g root "$SERVER_KEY" "$INSTALLED_KEY"

if [ ! -f "$BACKUP_FILE" ]; then
  sudo cp -a "$ENV_FILE" "$BACKUP_FILE"
fi

sudo sed -i \
  -e '/^SENTINEL_DASHBOARD_TLS_CERT=/d' \
  -e '/^SENTINEL_DASHBOARD_TLS_KEY=/d' \
  -e '/^SENTINEL_DASHBOARD_HOSTNAME=/d' \
  "$ENV_FILE"
{
  printf '\n'
  printf 'SENTINEL_DASHBOARD_TLS_CERT=%s\n' "$INSTALLED_CERT"
  printf 'SENTINEL_DASHBOARD_TLS_KEY=%s\n' "$INSTALLED_KEY"
  printf 'SENTINEL_DASHBOARD_HOSTNAME=127.0.0.1\n'
} | sudo tee -a "$ENV_FILE" >/dev/null
sudo chmod 600 "$ENV_FILE"
sudo chown root:root "$ENV_FILE"

certutil -N -d "sql:$NSS_DB" --empty-password
certutil -A -d "sql:$NSS_DB" -n "$ROOT_NICKNAME" -t "C,," -i "$ROOT_CERT"

echo "root_cert=$ROOT_CERT"
echo "installed_cert=$INSTALLED_CERT"
echo "installed_key=$INSTALLED_KEY"
echo "nss_db=sql:$NSS_DB"
certutil -L -d "sql:$NSS_DB" -n "$ROOT_NICKNAME"

sudo install -m 644 -o root -g root "$ROOT_CERT" "$SYSTEM_ROOT"
sudo update-ca-certificates
echo "system_root=$SYSTEM_ROOT"
openssl verify -CAfile "$ROOT_CERT" "$SERVER_CERT"
