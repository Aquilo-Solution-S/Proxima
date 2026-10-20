#!/usr/bin/env python3
"""Exercise Upgrade footer parsing and release-note rendering."""
from __future__ import annotations

import argparse
import json
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]


def cliff(*arguments: str) -> str:
    return subprocess.run(
        ["git", "cliff", "--offline", "--config", str(ROOT / "cliff.toml"), *arguments],
        cwd=ROOT, text=True, capture_output=True, check=True,
    ).stdout


def validate(releases: list[dict]) -> list[str]:
    failures = []
    for release in releases:
        for commit in release["commits"]:
            if not commit["breaking"]:
                continue
            notes = [item["value"] for item in commit["footers"] if item["token"] == "Upgrade"]
            if len(notes) != 1 or not notes[0].strip():
                failures.append(f'{commit["id"][:7]} {commit["message"]}: breaking commit requires one nonempty Upgrade: footer')
    return failures


def self_test() -> None:
    # Custom messages enter the real parser and Tera template without changing
    # Git history. Unique markers distinguish fixture text from repository notes.
    messages = [
        "feat(core)!: fixture break\n\nUpgrade: upgrade-fixture-retire-resource.\n continuation-line-kept.",
        "feat(core): fixture addition\n\nUpgrade: upgrade-fixture-nonbreaking-hidden.",
        "fix(core): fixture fix",
    ]
    args = ["--unreleased"]
    for message in messages:
        args += ["--with-commit", message]
    rendered = cliff(*args)
    assert rendered.count("upgrade-fixture-retire-resource.") == 1, rendered
    assert "continuation-line-kept." in rendered, rendered
    assert "upgrade-fixture-nonbreaking-hidden." not in rendered, rendered
    assert "### Upgrade" in rendered, rendered
    parsed = json.loads(cliff(*args, "--context"))
    fixtures = [{"commits": [commit for release in parsed for commit in release["commits"]
                              if commit["message"].startswith("fixture")]}]
    assert not validate(fixtures)
    missing = json.loads(cliff("--unreleased", "--context", "--with-commit", "feat(core)!: fixture missing"))
    bad = [commit for release in missing for commit in release["commits"]
           if commit["message"] == "fixture missing"]
    assert len(validate([{"commits": bad}])) == 1
    duplicated = json.loads(cliff("--unreleased", "--context", "--with-commit",
                                 "feat(core)!: fixture duplicate\n\nUpgrade: first\nUpgrade: second"))
    bad = [commit for release in duplicated for commit in release["commits"]
           if commit["message"] == "fixture duplicate"]
    assert len(validate([{"commits": bad}])) == 1
    # GitHub renders release notes as GFM, which drops `<word>` placeholders as
    # raw HTML. Outside code spans `<` is escaped; inside them it stays literal.
    placeholder = cliff("--unreleased", "--with-commit",
                        'feat(core)!: fixture placeholder\n\nUpgrade: write "<crate-name>/<short>",'
                        " keep `Vec<u8>` and `-> user`; a <tag> and 2 > 1.")
    assert '- write "&lt;crate-name>/&lt;short>", keep `Vec<u8>` and `-> user`; a &lt;tag> and 2 > 1.\n' \
        in placeholder, placeholder
    unmatched = cliff("--unreleased", "--with-commit",
                      "feat(core)!: fixture unmatched\n\nUpgrade: run `a <b> and <c>.")
    assert "- run `a &lt;b> and &lt;c>.\n" in unmatched, unmatched
    print("upgrade notes: parser, breaking-only rendering, continuation, missing, duplicate and placeholder footer fixtures passed")


def main() -> int:
    # Fixtures are the only mode: whether a breaking commit's Upgrade footer
    # is usable is review judgment (AGENTS.md), not something this script reads
    # from history. The flag is required so no caller mistakes it for a gate.
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--self-test", action="store_true", required=True)
    parser.parse_args()
    try:
        self_test()
        return 0
    except (OSError, subprocess.CalledProcessError, ValueError) as error:
        print(f"upgrade notes check failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
