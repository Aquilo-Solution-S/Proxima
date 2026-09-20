#!/usr/bin/env python3
"""Validate `proxima_core::RELEASE_VERSION` against the tags already published.

The tag IS the release. `release.yml` cuts `v${RELEASE_VERSION}` when a merge
to `main` carries a value no tag holds yet, and cuts nothing when the value is
unchanged — so this one constant is the release trigger. That makes it worth
checking on every PR rather than remembering at tag time, because the mistakes
it catches are only visible once the tag exists and the notes are written.

Two shapes pass:

  unchanged   `RELEASE_VERSION` already has a tag; merging cuts no release.
  bumped      `RELEASE_VERSION` is the next patch, minor or major of the
              highest tag; merging cuts exactly that tag.

Everything else fails — a version that skips a number, goes backwards, or
reuses a tag whose notes are already published and whose commit is already
named by a consumer's `Cargo.toml`.
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
from pathlib import Path

SOURCE = Path("crates/core/src/lib.rs")
CONST = re.compile(r'^pub const RELEASE_VERSION: &str = "([^"]*)";$', re.MULTILINE)
SEMVER = re.compile(r"^(\d+)\.(\d+)\.(\d+)$")


def read_release_version(text: str) -> str:
    found = CONST.search(text)
    if not found:
        raise ValueError(
            f"{SOURCE} declares no `pub const RELEASE_VERSION`; "
            "release.yml derives the tag from it and cannot run without it"
        )
    return found.group(1)


def parse(version: str) -> tuple[int, int, int] | None:
    found = SEMVER.match(version)
    if not found:
        return None
    return (int(found.group(1)), int(found.group(2)), int(found.group(3)))


def successors(latest: tuple[int, int, int]) -> list[tuple[int, int, int]]:
    """The three versions that may follow `latest`.

    Deliberately not "anything greater". A typo that lands 0.0.51 where 0.0.15
    was meant is still monotonic, and the number it skipped can never be used
    afterwards without going backwards.
    """
    major, minor, patch = latest
    return [(major, minor, patch + 1), (major, minor + 1, 0), (major + 1, 0, 0)]


def show(version: tuple[int, int, int]) -> str:
    return ".".join(str(part) for part in version)


def validate(current: str, tags: list[str]) -> tuple[bool, str]:
    parsed = parse(current)
    if parsed is None:
        return False, f"RELEASE_VERSION {current!r} is not a MAJOR.MINOR.PATCH version"

    released = sorted(filter(None, (parse(tag.removeprefix("v")) for tag in tags)))
    if not released:
        return True, f"no released tag yet; merging cuts v{current}"

    latest = released[-1]
    if parsed == latest:
        return True, f"RELEASE_VERSION is v{current}, already released; merging cuts no tag"

    if parsed in released:
        return False, (
            f"v{current} is already released. A tag names one commit forever — "
            f"its notes are written and consumers pin it. Next is v{show(successors(latest)[0])}"
        )

    allowed = successors(latest)
    if parsed not in allowed:
        return False, (
            f"v{current} does not follow the latest release v{show(latest)}. "
            f"Expected one of: {', '.join('v' + show(nxt) for nxt in allowed)}"
        )

    return True, f"RELEASE_VERSION bumped v{show(latest)} -> v{current}; merging cuts that tag"


def git_tags() -> list[str]:
    result = subprocess.run(
        ["git", "tag", "--list", "v*"],
        capture_output=True,
        text=True,
        check=True,
    )
    return [line.strip() for line in result.stdout.splitlines() if line.strip()]


def self_test() -> int:
    tags = ["v0.0.13", "v0.0.14", "not-a-version"]
    cases: list[tuple[str, list[str], bool, str]] = [
        ("0.0.14", tags, True, "unchanged from the latest release"),
        ("0.0.15", tags, True, "next patch"),
        ("0.1.0", tags, True, "next minor"),
        ("1.0.0", tags, True, "next major"),
        ("0.0.13", tags, False, "an earlier release, already tagged"),
        ("0.0.16", tags, False, "skips a patch"),
        ("0.2.0", tags, False, "skips a minor"),
        ("0.0.14-rc1", tags, False, "not MAJOR.MINOR.PATCH"),
        ("0.0.1", [], True, "first release, no tags yet"),
    ]
    failures = 0
    for current, available, expected, why in cases:
        ok, message = validate(current, available)
        if ok != expected:
            failures += 1
            print(f"self-test FAILED ({why}): {current} -> {ok}: {message}", file=sys.stderr)
    if failures:
        print(f"{failures} self-test case(s) failed", file=sys.stderr)
        return 1
    print(f"release-version self-test: {len(cases)} cases passed")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--self-test", action="store_true", help="check this script's own rules")
    args = parser.parse_args()

    if args.self_test:
        return self_test()

    try:
        current = read_release_version(SOURCE.read_text(encoding="utf-8"))
    except (OSError, ValueError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1

    ok, message = validate(current, git_tags())
    if not ok:
        print(f"error: {message}", file=sys.stderr)
        return 1
    print(message)
    return 0


if __name__ == "__main__":
    sys.exit(main())
