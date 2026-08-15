#!/usr/bin/env python3
"""Validate SQLx migration version reservations.

Core and flavors share SQLx's default `_sqlx_migrations` version namespace.
Runtime boot rejects duplicate versions before applying migrations; this check
keeps the documented source lanes honest at review time.
"""
from __future__ import annotations

import argparse
import re
import sys
import tempfile
from dataclasses import dataclass
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
MIGRATION_FILE = re.compile(r"^(?P<version>\d+)_[A-Za-z0-9][A-Za-z0-9_-]*\.sql$")


@dataclass(frozen=True)
class VersionLane:
    source: str
    path: str
    description: str
    min_version: int | None = None
    max_version: int | None = None
    suffix_min: int | None = None
    suffix_max: int | None = None

    def accepts(self, version: int) -> bool:
        if self.min_version is not None and version < self.min_version:
            return False
        if self.max_version is not None and version > self.max_version:
            return False
        if self.suffix_min is not None and self.suffix_max is not None:
            suffix = version % 100
            return self.suffix_min <= suffix <= self.suffix_max
        return True


LANES = [
    VersionLane(
        source="proxima-core",
        path="crates/storage-pg/migrations",
        description="reserved core/substrate integer lane",
        min_version=1,
        max_version=9_999,
    ),
    VersionLane(
        source="proxima-code",
        path="flavors/code/migrations",
        description="first-party flavor timestamp suffix lane 20-39",
        min_version=20_000_000_000_000,
        suffix_min=20,
        suffix_max=39,
    ),
]


@dataclass(frozen=True)
class MigrationVersion:
    source: str
    path: Path
    version: int
    lane: VersionLane

    def render(self, root: Path) -> str:
        rel = self.path.relative_to(root) if self.path.is_relative_to(root) else self.path
        return f"{self.source}:{rel}:{self.version}"


def collect(root: Path, lanes: list[VersionLane] = LANES) -> tuple[list[MigrationVersion], list[str]]:
    versions: list[MigrationVersion] = []
    diagnostics: list[str] = []
    for lane in lanes:
        base = root / lane.path
        if not base.exists():
            diagnostics.append(f"{lane.source}: missing migration directory {lane.path}")
            continue
        for path in sorted(base.glob("*.sql")):
            match = MIGRATION_FILE.fullmatch(path.name)
            rel = path.relative_to(root) if path.is_relative_to(root) else path
            if match is None:
                diagnostics.append(f"{rel}: migration filename must be <version>_<description>.sql")
                continue
            version = int(match.group("version"))
            item = MigrationVersion(lane.source, path, version, lane)
            versions.append(item)
            if not lane.accepts(version):
                diagnostics.append(
                    f"{item.render(root)} outside {lane.description}"
                )
    return versions, diagnostics


def validate(root: Path, lanes: list[VersionLane] = LANES) -> list[str]:
    versions, diagnostics = collect(root, lanes)
    seen: dict[int, MigrationVersion] = {}
    for item in versions:
        previous = seen.get(item.version)
        if previous is not None:
            diagnostics.append(
                f"duplicate migration version {item.version}: {previous.render(root)} and {item.render(root)}"
            )
        else:
            seen[item.version] = item
    return diagnostics


def run(root: Path) -> int:
    diagnostics = validate(root)
    if diagnostics:
        print("migration range check failed:", file=sys.stderr)
        for diagnostic in diagnostics:
            print(f"  {diagnostic}", file=sys.stderr)
        return 1
    versions, _ = collect(root)
    rendered = ", ".join(f"{item.source}:{item.version}" for item in sorted(versions, key=lambda v: v.version))
    print(f"migration range check OK: {rendered}")
    return 0


def write_fixture(root: Path, files: dict[str, list[str]]) -> None:
    for rel_dir, names in files.items():
        directory = root / rel_dir
        directory.mkdir(parents=True)
        for name in names:
            (directory / name).write_text("-- fixture\n", encoding="utf-8")


def self_test() -> int:
    cases = [
        (
            "current lanes accept disjoint versions",
            {
                "crates/storage-pg/migrations": ["0001_init.sql", "0008_v005.sql"],
                "flavors/code/migrations": ["20260801000020_v007_baseline.sql"],
            },
            False,
        ),
        (
            "duplicate versions fail",
            {
                "crates/storage-pg/migrations": ["0001_init.sql"],
                "flavors/code/migrations": [
                    "20260801000020_a.sql",
                    "20260801000020_b.sql",
                ],
            },
            True,
        ),
        (
            "wrong suffix lane fails",
            {
                "crates/storage-pg/migrations": ["0001_init.sql"],
                "flavors/code/migrations": [
                    "20260612000010_baseline.sql",
                    "20260801000020_v007_baseline.sql",
                ],
            },
            True,
        ),
    ]
    failures: list[str] = []
    for name, files, should_fail in cases:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            write_fixture(root, files)
            failed = bool(validate(root))
            if failed != should_fail:
                failures.append(f"{name}: expected fail={should_fail}")
    if failures:
        print("migration range self-test failed:", file=sys.stderr)
        for failure in failures:
            print(f"  {failure}", file=sys.stderr)
        return 1
    print("migration range self-test OK")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--self-test", action="store_true", help="run fixture checks")
    parser.add_argument("--root", type=Path, default=ROOT)
    args = parser.parse_args()
    if args.self_test:
        return self_test()
    return run(args.root.resolve())


if __name__ == "__main__":
    raise SystemExit(main())
