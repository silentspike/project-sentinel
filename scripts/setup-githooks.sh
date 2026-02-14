#!/usr/bin/env bash
set -euo pipefail

repo_root="$(git rev-parse --show-toplevel)"
cd "$repo_root"

if [[ ! -d ".githooks" ]]; then
  echo "ERROR: .githooks directory missing"
  exit 1
fi

chmod +x .githooks/*
git config core.hooksPath .githooks

echo "Git hooks installed via core.hooksPath=.githooks"
echo "Active hook files:"
ls -1 .githooks
