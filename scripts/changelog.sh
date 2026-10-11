#!/usr/bin/env bash
# Preview release notes from Conventional Commit messages via git-cliff.
#
# This is a read-only local preview. GitHub Releases are the canonical release
# note store; the tracked CHANGELOG.md only points there. Merging a
# RELEASE_VERSION bump to main makes release.yml create the tag and generate
# its notes.
set -euo pipefail

if ! command -v git-cliff >/dev/null 2>&1; then
  echo "error: git-cliff not found — install with: brew install git-cliff" >&2
  exit 1
fi

cd "$(git rev-parse --show-toplevel)"

if [ $# -gt 0 ]; then
  git cliff --offline --tag "$1" --unreleased --strip header
else
  git cliff --offline --unreleased --strip header
fi
