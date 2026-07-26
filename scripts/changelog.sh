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

# An unreleased tag has no commits pointing at it yet, so git-cliff would file
# this release's commits under "unreleased". Passing --tag stamps them under
# the version being cut — which is the whole point of running this before the
# tag exists.
if [ $# -gt 0 ]; then
  git cliff --tag "$1" --output CHANGELOG.md
  echo "CHANGELOG.md regenerated for $1."
else
  git cliff --output CHANGELOG.md
  echo "CHANGELOG.md regenerated (no tag given; this release lands under 'unreleased')."
  echo "hint: scripts/changelog.sh vX.Y.Z stamps it under the version you are cutting." >&2
fi
