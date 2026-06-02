#!/usr/bin/env bash
#
# scripts/fetch-demo-binaries.sh — pull pre-built demo binaries from the
# GitHub Release tagged matching `git describe --tags --abbrev=0` (default
# v0.1.0-alpha) into ./target/release/.
#
# Used by the Makefile `demo-binaries` target as the cheapest path: no
# Rust toolchain needed, no cargo-remote needed. ~60 MB download.
#
# Usage:
#   ./scripts/fetch-demo-binaries.sh
#
# Knobs:
#   SENTINEL_RELEASE_TAG   override the tag to fetch (default: v0.1.0-alpha)
#   GH_REPO                override the repo (default: silentspike/project-sentinel)
#
# Behavior:
# - exit 0  binaries already present (skip download)
# - exit 0  download succeeded; binaries placed in target/release/
# - exit 1  download failed (gh missing, no auth, no release, network)

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

tag="${SENTINEL_RELEASE_TAG:-v0.1.0-alpha}"
gh_repo="${GH_REPO:-silentspike/project-sentinel}"
out_dir="target/release"
binaries=(sentinel-daemon sentinel-nightrun sentinel-projection sentinel-dashboard-backend)

note() { printf '[fetch-demo-binaries] %s\n' "$*"; }
fail() { printf '[fetch-demo-binaries] FAIL: %s\n' "$*" >&2; exit 1; }

# Skip if all binaries are already present
all_present=true
for b in "${binaries[@]}"; do
    [ -x "$out_dir/$b" ] || { all_present=false; break; }
done
if "$all_present"; then
    note "binaries already present in $out_dir/, skipping download"
    exit 0
fi

# Need gh + auth
command -v gh >/dev/null 2>&1 || fail "'gh' (GitHub CLI) not found on PATH. Install: https://cli.github.com/"
gh auth status >/dev/null 2>&1 || fail "'gh' not authenticated. Run: gh auth login"

mkdir -p "$out_dir"
work=$(mktemp -d -t sentinel-fetch-XXXXXX)
trap 'rm -rf "$work"' EXIT

note "fetching $tag from $gh_repo into $work..."
if ! gh release download "$tag" \
        --repo "$gh_repo" \
        --dir "$work" \
        --pattern 'sentinel-daemon' \
        --pattern 'sentinel-nightrun' \
        --pattern 'sentinel-projection' \
        --pattern 'sentinel-dashboard-backend' \
        --clobber 2>&1 | tail -5; then
    fail "gh release download failed"
fi

for b in "${binaries[@]}"; do
    [ -f "$work/$b" ] || fail "asset '$b' missing in release $tag"
    install -m 0755 "$work/$b" "$out_dir/$b"
    note "installed $out_dir/$b ($(stat -c %s "$out_dir/$b") bytes)"
done

note "done — binaries ready in $out_dir/"
