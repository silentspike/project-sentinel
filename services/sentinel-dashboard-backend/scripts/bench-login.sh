#!/usr/bin/env bash
# #474 login benchmark — run ON the deploy VM (never via cargo remote).
#
# Measures POST /api/auth/login latency and the rate-limit behavior. The TLS handshake dominates
# `time_total`, so the limiter overhead (ns-level) is invisible in absolute latency; the meaningful
# signals are (a) wrong vs. wrong-prefix latency (constant-time stays effective) and (b) the 429
# fast-path + recovery, not a before/after delta.
#
# The key is read from the BENCH_KEY environment variable (never passed as an argv, so it does not
# show up in the process list on a multi-user host). Output is key-free.
#
# Usage:
#   BENCH_KEY=<key> ./bench-login.sh <mode> [n] [url]
# Modes:
#   success       n logins with the correct key            -> p50/p95
#   wrong-key     n logins with a fully wrong key          -> p50/p95
#   wrong-prefix  n logins with all-but-last-byte correct  -> p50/p95 (constant-time probe)
#   sweep-429     count attempts until the first 429, print Retry-After
#
# Defaults: n=100, url=https://127.0.0.1:8001

set -euo pipefail

MODE="${1:-success}"
N="${2:-100}"
URL="${3:-https://127.0.0.1:8001}/api/auth/login"

if [ -z "${BENCH_KEY:-}" ]; then
  echo "BENCH_KEY env var is required (the operator key; never pass it as an argument)" >&2
  exit 2
fi

# Build the body for the requested mode without printing the key.
wrong_prefix_key() {
  # All bytes of the key are correct except the last one -> same length, single trailing diff.
  local k="$BENCH_KEY"
  local head="${k:0:${#k}-1}"
  local last="${k: -1}"
  local repl='X'
  [ "$last" = "X" ] && repl='Y'
  printf '%s%s' "$head" "$repl"
}

post_body() {
  # $1 = key value; emit time_total (success/wrong modes) via curl -w
  curl -sk -o /dev/null -w '%{time_total}\n' -X POST "$URL" \
    -H 'content-type: application/json' --data @- <<JSON
{"key":"$1"}
JSON
}

percentile() { # $1=file $2=percentile(0-100)
  sort -n "$1" | awk -v p="$2" '{a[NR]=$1} END{n=NR; if(n==0){print "n/a"; exit} idx=int(p*n/100); if(idx<1)idx=1; printf "%.4f", a[idx]}'
}

case "$MODE" in
  success|wrong-key|wrong-prefix)
    case "$MODE" in
      success)      KEY="$BENCH_KEY" ;;
      wrong-key)    KEY="zzzz-wrong-zzzz" ;;
      wrong-prefix) KEY="$(wrong_prefix_key)" ;;
    esac
    TMP="$(mktemp)"
    for _ in $(seq 1 "$N"); do post_body "$KEY" >> "$TMP"; done
    echo "mode=$MODE n=$N  p50=$(percentile "$TMP" 50)s  p95=$(percentile "$TMP" 95)s"
    rm -f "$TMP"
    ;;
  sweep-429)
    attempt=0
    while [ "$attempt" -lt 1000 ]; do
      attempt=$((attempt + 1))
      resp="$(curl -sk -o /dev/null -D - -w '\nHTTP %{http_code}' -X POST "$URL" \
        -H 'content-type: application/json' --data '{"key":"zzzz-wrong-zzzz"}' 2>/dev/null || true)"
      code="$(printf '%s' "$resp" | awk '/^HTTP [0-9]/{print $2}' | tail -1)"
      if [ "$code" = "429" ]; then
        retry="$(printf '%s' "$resp" | awk 'BEGIN{IGNORECASE=1}/^retry-after:/{print $2}' | tr -d '\r')"
        echo "first 429 at attempt=$attempt retry_after=${retry:-?}s"
        exit 0
      fi
    done
    echo "no 429 within 1000 attempts (limiter not engaging?)"
    ;;
  *)
    echo "unknown mode: $MODE (success|wrong-key|wrong-prefix|sweep-429)" >&2
    exit 2
    ;;
esac
