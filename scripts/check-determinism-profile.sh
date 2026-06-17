#!/usr/bin/env bash
# check-determinism-profile.sh — Verifies the DEV-010 determinism profile preconditions.
#
# Part of the #494 determinism profile: the build toolchain is pinned and no
# fast-math / FMA-contraction / target-feature override is active that would break
# homogeneous replay determinism (TM-3 #491, homogeneous cross-node migration #501).
# Prints evidence; exits non-zero on any violation.
#
# Usage: bash scripts/check-determinism-profile.sh
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
FAIL=0

echo "== DEV-010 determinism profile check =="

# 1) Pinned toolchain (rust-toolchain.toml is the SSOT for the pinned channel)
EXPECT="$(grep -oE 'channel[[:space:]]*=[[:space:]]*"[^"]+"' "${REPO_ROOT}/rust-toolchain.toml" \
  | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' || true)"
if [ -z "${EXPECT}" ]; then
  echo "FAIL: rust-toolchain.toml does not pin an explicit channel version"
  FAIL=1
else
  echo "PASS: rust-toolchain.toml pins channel ${EXPECT}"
fi

# 2) Effective rustc matches the pin (if rustc is on PATH)
if command -v rustc >/dev/null 2>&1; then
  ACTUAL="$(rustc --version | awk '{print $2}')"
  if [ -n "${EXPECT}" ] && [ "${ACTUAL}" != "${EXPECT}" ]; then
    echo "FAIL: effective rustc ${ACTUAL} != pinned ${EXPECT}"
    FAIL=1
  else
    echo "PASS: effective rustc ${ACTUAL} matches the pin"
  fi
else
  echo "INFO: rustc not on PATH (skipping effective-version check)"
fi

# 3) No fast-math / FMA-contraction / target-feature override in RUSTFLAGS or cargo config
BAD_PATTERN='ffast-math|fast-math|fp-contract|target-feature=|-Cllvm-args|llvm-args'
declare -a SOURCES=()
[ -n "${RUSTFLAGS:-}" ] && SOURCES+=("env RUSTFLAGS: ${RUSTFLAGS}")
[ -n "${CARGO_ENCODED_RUSTFLAGS:-}" ] && SOURCES+=("env CARGO_ENCODED_RUSTFLAGS: ${CARGO_ENCODED_RUSTFLAGS}")
for cfg in "${REPO_ROOT}/.cargo/config.toml" "${REPO_ROOT}/.cargo/config"; do
  [ -f "${cfg}" ] && SOURCES+=("file ${cfg}: $(tr '\n' ' ' < "${cfg}")")
done
HIT=0
if [ "${#SOURCES[@]}" -gt 0 ]; then
  for s in "${SOURCES[@]}"; do
    if printf '%s' "${s}" | grep -qiE "${BAD_PATTERN}"; then
      echo "FAIL: determinism-breaking flag detected -> ${s}"
      HIT=1
      FAIL=1
    fi
  done
fi
if [ "${HIT}" = "0" ]; then
  echo "PASS: no fast-math / FMA / target-feature override in RUSTFLAGS or cargo config"
fi

# 4) Evidence: the pinned toolchain content
echo "INFO: rust-toolchain.toml:"
sed 's/^/    /' "${REPO_ROOT}/rust-toolchain.toml"

if [ "${FAIL}" = "0" ]; then
  echo "DEV-010 determinism profile: OK"
  exit 0
else
  echo "DEV-010 determinism profile: VIOLATIONS FOUND"
  exit 1
fi
