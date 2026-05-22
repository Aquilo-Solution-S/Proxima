#!/usr/bin/env bash
set -euo pipefail

# Builds both images the per-wake observation sandbox needs: the workspace
# container (build/test tooling) and the egress logging proxy.
sandbox_image="${1:-proxima-workspace-sandbox:local}"
proxy_image="${2:-proxima-workspace-proxy:local}"
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

docker build \
  -f "$repo_root/scripts/workspace-shell-sandbox.Dockerfile" \
  -t "$sandbox_image" \
  "$repo_root"

docker build \
  -f "$repo_root/scripts/workspace-proxy.Dockerfile" \
  -t "$proxy_image" \
  "$repo_root"
