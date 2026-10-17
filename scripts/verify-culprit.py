#!/usr/bin/env python3
"""Find the commit that turned `main` red, for `.github/workflows/verify.yml`.

`verified` is the last commit that passed CI; `main` may run ahead of it. When
CI fails on the newest `main` commit, verify re-runs CI on a sample of the
commits between the two (the probes) and asks this script for a verdict:

  revert <sha>        the first red probe sits directly on a green one, so that
                      commit broke `main`; verify reverts it.
  range <from> <to>   the first red probe's parent was not probed; the break
                      lies in (from, to]. Verify opens an issue.
  healed              the newest commit no longer fails; the first run was a
                      flake or a later commit fixed it. Verify opens an issue.

Subcommands:

  sample --base B --tip T      JSON list of up to MAX_PROBES first-parent
                               commits in B..T, oldest first, tip included.
  judge --base B --probes P --jobs J
                               verdict from the probe list P and the
                               `gh run view --json jobs` output J.
"""

from __future__ import annotations

import argparse
import enum
import json
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path

MAX_PROBES = 8
# verify.yml names each probe job `probe <sha>`; the jobs it calls from
# ci.yml show up as `probe <sha> / <job name>`.
PROBE_PREFIX = "probe "


class Outcome(enum.Enum):
    GREEN = "green"
    RED = "red"
    UNKNOWN = "unknown"


# GitHub's job conclusions, sorted once. A conclusion GitHub adds later
# reads as unknown, so it can never trigger a revert on its own.
GREEN_CONCLUSIONS = frozenset({"success", "skipped", "neutral"})
RED_CONCLUSIONS = frozenset({"failure", "timed_out", "startup_failure"})


@dataclass(frozen=True)
class Revert:
    culprit: str


@dataclass(frozen=True)
class Range:
    after: str
    upto: str


@dataclass(frozen=True)
class Healed:
    pass


Verdict = Revert | Range | Healed


def git(*args: str) -> str:
    return subprocess.run(
        ["git", *args], check=True, capture_output=True, text=True
    ).stdout.strip()


def sample(commits: list[str], limit: int = MAX_PROBES) -> list[str]:
    """Evenly spaced commits, oldest first, always ending with the tip."""
    if not commits:
        raise ValueError("no commits between verified and the tip")
    if len(commits) <= limit:
        return commits
    step = len(commits) / limit
    picked = {commits[round((i + 1) * step) - 1] for i in range(limit)}
    return [c for c in commits if c in picked]


def probe_outcomes(probes: list[str], jobs: list[dict]) -> dict[str, Outcome]:
    conclusions: dict[str, list[str | None]] = {sha: [] for sha in probes}
    for job in jobs:
        caller, _, _ = job["name"].partition(" / ")
        if not caller.startswith(PROBE_PREFIX):
            continue
        sha = caller.removeprefix(PROBE_PREFIX)
        if sha in conclusions:
            conclusions[sha].append(job.get("conclusion"))
    return {sha: outcome(found) for sha, found in conclusions.items()}


def outcome(conclusions: list[str | None]) -> Outcome:
    if any(c in RED_CONCLUSIONS for c in conclusions):
        return Outcome.RED
    if conclusions and all(c in GREEN_CONCLUSIONS for c in conclusions):
        return Outcome.GREEN
    return Outcome.UNKNOWN


def judge(
    base: str, probes: list[str], outcomes: dict[str, Outcome], parent_of
) -> Verdict:
    match outcomes[probes[-1]]:
        case Outcome.RED:
            pass
        case Outcome.GREEN | Outcome.UNKNOWN:
            return Healed()
    last_green = base
    for sha in probes:
        match outcomes[sha]:
            case Outcome.GREEN:
                last_green = sha
            case Outcome.UNKNOWN:
                continue
            case Outcome.RED:
                if parent_of(sha) == last_green:
                    return Revert(sha)
                return Range(last_green, sha)
    raise AssertionError("the tip is red, so some probe is red")


def emit(verdict: Verdict) -> str:
    match verdict:
        case Revert(culprit):
            return f"verdict=revert\nculprit={culprit}\n"
        case Range(after, upto):
            return f"verdict=range\nafter={after}\nupto={upto}\n"
        case Healed():
            return "verdict=healed\n"


def self_test() -> None:
    commits = [f"c{i}" for i in range(20)]
    picked = sample(commits)
    assert len(picked) == MAX_PROBES and picked[-1] == "c19", picked
    assert sample(commits[:3]) == commits[:3]

    jobs = [
        {"name": "probe c1 / test (postgres + pgvector)", "conclusion": "success"},
        {"name": "probe c1 / changed paths", "conclusion": "success"},
        {"name": "probe c2 / test (postgres + pgvector)", "conclusion": "failure"},
        {"name": "probe c3 / test (postgres + pgvector)", "conclusion": "cancelled"},
        {"name": "probe c4 / docs links + status + lean", "conclusion": "failure"},
        {"name": "culprit", "conclusion": None},
    ]
    found = probe_outcomes(["c1", "c2", "c3", "c4", "c5"], jobs)
    assert found == {
        "c1": Outcome.GREEN,
        "c2": Outcome.RED,
        "c3": Outcome.UNKNOWN,
        "c4": Outcome.RED,
        "c5": Outcome.UNKNOWN,
    }, found

    linear = {"c1": "base", "c2": "c1", "c3": "c2", "c4": "c3"}.__getitem__
    G, R, U = Outcome.GREEN, Outcome.RED, Outcome.UNKNOWN
    cases = [
        ({"c1": G, "c2": R, "c4": R}, ["c1", "c2", "c4"], Revert("c2")),
        ({"c1": R, "c4": R}, ["c1", "c4"], Revert("c1")),
        ({"c2": G, "c4": R}, ["c2", "c4"], Range("c2", "c4")),
        ({"c1": U, "c2": R, "c4": R}, ["c1", "c2", "c4"], Range("base", "c2")),
        ({"c2": R, "c4": G}, ["c2", "c4"], Healed()),
        ({"c4": U}, ["c4"], Healed()),
    ]
    for outcomes, probes, expected in cases:
        got = judge("base", probes, outcomes, linear)
        assert got == expected, (outcomes, got, expected)

    assert emit(Revert("c2")) == "verdict=revert\nculprit=c2\n"
    print("verify-culprit self-test: ok")


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Find the commit that turned main red."
    )
    parser.add_argument("--self-test", action="store_true")
    sub = parser.add_subparsers(dest="command")
    s = sub.add_parser("sample")
    s.add_argument("--base", required=True)
    s.add_argument("--tip", required=True)
    j = sub.add_parser("judge")
    j.add_argument("--base", required=True)
    j.add_argument("--probes", required=True, help="JSON list from `sample`")
    j.add_argument("--jobs", required=True, type=Path)
    args = parser.parse_args()

    if args.self_test:
        self_test()
        return 0
    if args.command == "sample":
        commits = git(
            "rev-list", "--reverse", "--first-parent", f"{args.base}..{args.tip}"
        ).split()
        print(json.dumps(sample(commits)))
        return 0
    if args.command == "judge":
        probes = json.loads(args.probes)
        jobs = json.loads(args.jobs.read_text())["jobs"]
        verdict = judge(
            args.base,
            probes,
            probe_outcomes(probes, jobs),
            lambda sha: git("rev-parse", f"{sha}^"),
        )
        sys.stdout.write(emit(verdict))
        return 0
    parser.print_help()
    return 2


if __name__ == "__main__":
    sys.exit(main())
