#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${ROOT}"

mapfile -t rust_components < <(
  find crates services -name Cargo.toml \
    | sed 's#/Cargo.toml##' \
    | grep -E '^(crates/sentinel-|services/)' \
    | grep -v '^crates/sentinel-wasm/tests/fixtures/' \
    | sort -u
)

mapfile -t go_components < <(
  find cmd pkg services -name go.mod \
    | sed 's#/go.mod##' \
    | sort -u
)

mapfile -t components < <(
  {
    printf '%s\n' "${rust_components[@]}"
    printf '%s\n' "${go_components[@]}"
  } | sort -u
)

missing=0
for component in "${components[@]}"; do
  readme="${component}/README.md"
  if [[ ! -f "${readme}" ]]; then
    echo "missing README: ${readme}" >&2
    missing=1
    continue
  fi

  for heading in "## Purpose" "## Interfaces" "## Dependencies" "## Verify"; do
    if ! grep -Fxq "${heading}" "${readme}"; then
      echo "missing heading ${heading}: ${readme}" >&2
      missing=1
    fi
  done

  if ! grep -Fq "${readme}" docs/component-readmes.md; then
    echo "missing index link: ${readme}" >&2
    missing=1
  fi
done

echo "component READMEs: ${#components[@]} total (${#rust_components[@]} Rust, ${#go_components[@]} Go)"

if [[ "${missing}" -ne 0 ]]; then
  exit 1
fi
