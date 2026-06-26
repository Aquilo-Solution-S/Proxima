#!/usr/bin/env bash
# Regenerate CHANGELOG.md from Conventional Commit messages via git-cliff.
#
# Reuses your existing `gh` login for GitHub enrichment (PR links, authors,
# first-time contributors) — no separate token to manage. Run before cutting a
# tag so the committed CHANGELOG.md is current; CI regenerates the per-tag
# Release notes independently (.github/workflows/release.yml).
set -euo pipefail

if ! command -v git-cliff >/dev/null 2>&1; then
  echo "error: git-cliff not found — install with: brew install git-cliff" >&2
  exit 1
fi

# [remote.github] enrichment needs a token. Reuse gh's so there's nothing to
# manage; never export an EMPTY token (git-cliff panics on a 401 for "").
if command -v gh >/dev/null 2>&1 && gh auth status >/dev/null 2>&1; then
  GITHUB_TOKEN="$(gh auth token)"
  export GITHUB_TOKEN
else
  echo "warn: no gh auth — generating without PR/author enrichment" >&2
  unset GITHUB_TOKEN || true
fi

cd "$(git rev-parse --show-toplevel)"
git cliff --output CHANGELOG.md
echo "CHANGELOG.md regenerated."
