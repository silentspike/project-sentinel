#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

command -v cargo-kani >/dev/null 2>&1 || {
  echo "cargo-kani is required" >&2
  exit 127
}

command -v cbmc >/dev/null 2>&1 || {
  echo "cbmc is required" >&2
  exit 127
}

echo "cargo-kani: $(cargo kani --version)"
echo "cbmc: $(cbmc --version | head -1)"

run_kani() {
  local crate_dir="$1"
  local unwind="$2"
  echo
  echo "== Kani: ${crate_dir} =="
  (
    cd "${ROOT}/${crate_dir}"
    cargo kani --output-format terse --default-unwind "${unwind}"
  )
}

run_kani "crates/sentinel-bio" 8
run_kani "crates/sentinel-common" 32
run_kani "crates/sentinel-limbo" 8
