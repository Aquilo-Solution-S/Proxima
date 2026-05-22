#!/usr/bin/env bash
set -euo pipefail

image="${1:-proxima-workspace-sandbox:local}"
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

docker build \
  -f "$repo_root/scripts/workspace-shell-sandbox.Dockerfile" \
  -t "$image" \
  "$repo_root"
