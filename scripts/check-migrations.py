#!/usr/bin/env python3
"""Check migration immutability and monotonic versions.

Released migration files are pinned by release tags from ``RELEASE_EPOCH``.
Files added after the latest release must have a version greater than every
version previously released in the same migration directory, and no two files
in a directory may share a version: agents push to `main` in parallel, and two
new files with one number rebase without a conflict. Definitions are edited in
place and are deliberately outside this check.
"""
from __future__ import annotations

import argparse
import os
import re
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
RELEASE_EPOCH = "v0.0.48"
RELEASE_VERSION_SOURCE = "crates/core/src/lib.rs"
RELEASE_VERSION_CONST = re.compile(
    r'^pub const RELEASE_VERSION: &str = "(?P<version>\d+\.\d+\.\d+)";$',
    re.MULTILINE,
)
RELEASE_TAG = re.compile(r"^v(?P<major>\d+)\.(?P<minor>\d+)\.(?P<patch>\d+)$")
MIGRATION_FILE = re.compile(r"^(?P<version>\d+)_[A-Za-z0-9][A-Za-z0-9_-]*\.sql$")
MIGRATION_ROOT = re.compile(r"^(?:crates/storage-pg/migrations|flavors/[^/]+/migrations)/")
FLAVOR_MIGRATION_DIR = re.compile(r"^flavors/[^/]+/migrations$")


def retired_lanes(root: Path, paths: set[str]) -> dict[str, int]:
    """Flavor migration directories with no .sql file left at HEAD, with their released file count.

    Git history keeps a retired lane's released files and nothing at HEAD can
    apply them. The core directory never retires, and a flavor directory that
    still holds any migration keeps every released file unchanged.
    """
    by_dir: dict[str, int] = {}
    for path in paths:
        directory = str(Path(path).parent)
        if FLAVOR_MIGRATION_DIR.fullmatch(directory):
            by_dir[directory] = by_dir.get(directory, 0) + 1
    return {
        directory: count
        for directory, count in sorted(by_dir.items())
        if not any((root / directory).glob("*.sql"))
    }


def git(root: Path, *args: str, env: dict[str, str] | None = None, stdin: str | None = None) -> str:
    result = subprocess.run(
        ["git", *args], cwd=root, env=env, input=stdin, capture_output=True, text=True, check=True
    )
    return result.stdout


def release_tags(root: Path, env: dict[str, str] | None = None) -> list[tuple[tuple[int, int, int], str]]:
    tags: list[tuple[tuple[int, int, int], str]] = []
    for tag in git(root, "tag", "--list", "v*", env=env).split():
        match = RELEASE_TAG.fullmatch(tag)
        if match:
            tags.append(((int(match["major"]), int(match["minor"]), int(match["patch"])), tag))
    return sorted(tags)


def tagged_migrations(root: Path, tag: str, env: dict[str, str] | None = None) -> dict[str, str]:
    files: dict[str, str] = {}
    for entry in git(root, "ls-tree", "-r", "-z", "--full-tree", tag, env=env).split("\0"):
        if not entry:
            continue
        meta, path = entry.split("\t", 1)
        _mode, kind, blob = meta.split()
        if kind == "blob" and MIGRATION_ROOT.match(path) and path.endswith(".sql"):
            files[path] = blob
    return files


def head_blobs(root: Path, paths: set[str], env: dict[str, str] | None = None) -> dict[str, str | None]:
    present = [path for path in paths if (root / path).is_file()]
    blobs: dict[str, str | None] = dict.fromkeys(paths)
    if present:
        output = git(root, "hash-object", "--stdin-paths", env=env, stdin="\n".join(present) + "\n")
        blobs.update(zip(present, output.split(), strict=True))
    return blobs


def pending_release(root: Path) -> str | None:
    try:
        source = (root / RELEASE_VERSION_SOURCE).read_text(encoding="utf-8")
    except OSError:
        return None
    match = RELEASE_VERSION_CONST.search(source)
    return f"v{match['version']}" if match else None


def check_released_migrations(
    root: Path,
    *,
    epoch: str = RELEASE_EPOCH,
    pending: str | None = None,
    env: dict[str, str] | None = None,
) -> tuple[list[str], str]:
    """Check every migration shipped by a tag at or after ``epoch``."""
    try:
        tags = release_tags(root, env)
    except (OSError, subprocess.CalledProcessError) as error:
        return [f"cannot list release tags ({error}); run from a git checkout"], ""
    epoch_match = RELEASE_TAG.fullmatch(epoch)
    if epoch_match is None:
        return [f"release epoch {epoch!r} is not a vMAJOR.MINOR.PATCH tag"], ""
    floor = tuple(int(epoch_match[name]) for name in ("major", "minor", "patch"))
    tag_names = {tag for _, tag in tags}
    if epoch not in tag_names:
        pending = pending_release(root) if pending is None else pending
        if not tags or epoch != pending or floor <= tags[-1][0]:
            return [
                f"release tag {epoch} is not in this clone; fetch tags "
                "(actions/checkout `fetch-tags: true`, or `git fetch --tags`)"
            ], ""
        return [], f"released migrations: {epoch} is the release this change cuts; no tag pins it yet"

    checked = [tag for version, tag in tags if version >= floor]
    tagged: dict[str, dict[str, str]] = {}
    for tag in checked:
        try:
            tagged[tag] = tagged_migrations(root, tag, env)
        except subprocess.CalledProcessError as error:
            return [f"{tag}: cannot read the tag's tree ({error}); fetch tags"], ""
    all_shipped = {path for files in tagged.values() for path in files}
    retired = retired_lanes(root, all_shipped)
    diagnostics: list[str] = []
    for tag in checked:
        shipped = {
            path: blob
            for path, blob in tagged[tag].items()
            if str(Path(path).parent) not in retired
        }
        now = head_blobs(root, set(shipped), env)
        diagnostics.extend(
            f"{path}: released blob {blob} in {tag} differs at HEAD — restore it and ship a new migration"
            for path, blob in sorted(shipped.items())
            if now[path] != blob
        )
    retired_count = sum(retired.values())
    summary = (
        f"released migrations unchanged: {len(all_shipped) - retired_count} files across "
        f"{len(checked)} tags ({checked[0]}..{checked[-1]})"
        if checked
        else ""
    )
    if retired:
        summary += "\nretired lanes: " + ", ".join(
            f"{directory} ({count} released files)" for directory, count in retired.items()
        )
    return diagnostics, summary


def migration_files(root: Path) -> dict[str, tuple[str, int]]:
    files: dict[str, tuple[str, int]] = {}
    bases = [root / "crates/storage-pg/migrations", *sorted((root / "flavors").glob("*/migrations"))]
    for base in bases:
        if not base.is_dir():
            continue
        source = str(base.relative_to(root))
        for path in sorted(base.glob("*.sql")):
            match = MIGRATION_FILE.fullmatch(path.name)
            if match:
                files[str(path.relative_to(root))] = (source, int(match["version"]))
    return files


def check_new_versions(root: Path, env: dict[str, str] | None = None) -> list[str]:
    tags = release_tags(root, env)
    if not tags:
        return ["no release tags found; cannot establish released migration versions"]
    released_paths: set[str] = set()
    released_max: dict[str, int] = {}
    for _version, tag in tags:
        for path in tagged_migrations(root, tag, env):
            released_paths.add(path)
            match = MIGRATION_FILE.fullmatch(Path(path).name)
            if match:
                directory = str(Path(path).parent)
                released_max[directory] = max(released_max.get(directory, 0), int(match["version"]))
    diagnostics: list[str] = []
    for path, (directory, version) in migration_files(root).items():
        if path in released_paths:
            continue
        maximum = released_max.get(directory, 0)
        if version <= maximum:
            diagnostics.append(
                f"{path}: new version {version} must be above released version {maximum} in {directory}"
            )
    return diagnostics


def check_unique_versions(root: Path) -> list[str]:
    paths: dict[tuple[str, int], list[str]] = {}
    for path, key in migration_files(root).items():
        paths.setdefault(key, []).append(path)
    return [
        f"{directory}: version {version} is used by {', '.join(sorted(found))}; renumber the newer file"
        for (directory, version), found in sorted(paths.items())
        if len(found) > 1
    ]


def run(root: Path) -> int:
    released, summary = check_released_migrations(root)
    diagnostics = released + check_new_versions(root) + check_unique_versions(root)
    if diagnostics:
        print("migration check failed:", file=sys.stderr)
        for diagnostic in diagnostics:
            print(f"  {diagnostic}", file=sys.stderr)
        return 1
    print("migration check OK")
    if summary:
        print(summary)
    return 0


class FixtureRepo:
    def __init__(self, root: Path) -> None:
        self.root = root
        self.env = {key: value for key, value in os.environ.items() if not key.startswith("GIT_")}
        self.env.update(
            GIT_CONFIG_GLOBAL=os.devnull,
            GIT_CONFIG_NOSYSTEM="1",
            GIT_AUTHOR_NAME="fixture",
            GIT_AUTHOR_EMAIL="fixture@example.invalid",
            GIT_COMMITTER_NAME="fixture",
            GIT_COMMITTER_EMAIL="fixture@example.invalid",
        )
        self.git("init", "-q", "-b", "main")

    def git(self, *args: str) -> str:
        return git(self.root, *args, env=self.env)

    def write(self, path: str, content: str) -> None:
        target = self.root / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(content, encoding="utf-8")

    def release(self, tag: str) -> None:
        self.git("add", "--all")
        self.git("commit", "-q", "--allow-empty", "-m", tag)
        self.git("tag", "-a", tag, "-m", tag)


def retired_lane_self_test() -> list[str]:
    """A wholly removed flavor migration directory is a retired lane; nothing else is."""
    failures: list[str] = []
    lane = "flavors/example/migrations"

    def released(files: list[str]) -> tuple[tempfile.TemporaryDirectory, FixtureRepo]:
        tmp = tempfile.TemporaryDirectory()
        repo = FixtureRepo(Path(tmp.name))
        for path in files:
            repo.write(path, f"-- {path}\n")
        repo.release("v0.0.36")
        return tmp, repo

    def check(repo: FixtureRepo) -> tuple[list[str], str]:
        return check_released_migrations(repo.root, epoch="v0.0.36", env=repo.env)

    # retired_flavor_lane_passes
    tmp, repo = released([f"{lane}/20261001000011_a.sql"])
    with tmp:
        repo.git("rm", "-q", "-r", lane)
        diagnostics, summary = check(repo)
        if diagnostics or "retired lanes" not in summary:
            failures.append(f"a wholly removed flavor lane should pass as retired: {diagnostics}")
    # partial_flavor_deletion_fails
    tmp, repo = released([f"{lane}/20261001000011_a.sql", f"{lane}/20261001000012_b.sql"])
    with tmp:
        repo.git("rm", "-q", f"{lane}/20261001000012_b.sql")
        if not check(repo)[0]:
            failures.append("removing one released file of a live flavor lane should fail")
    # core_lane_never_retires
    tmp, repo = released(["crates/storage-pg/migrations/0001_x.sql"])
    with tmp:
        repo.git("rm", "-q", "-r", "crates/storage-pg/migrations")
        if not check(repo)[0]:
            failures.append("the core migration directory must never retire")
    # revived_lane_must_restore
    tmp, repo = released([f"{lane}/20261001000011_a.sql"])
    with tmp:
        repo.git("rm", "-q", "-r", lane)
        repo.write(f"{lane}/20261001000013_new.sql", "-- revived\n")
        if not check(repo)[0]:
            failures.append("a revived lane must restore its released files first")
    return failures


def self_test() -> int:
    failures: list[str] = []
    with tempfile.TemporaryDirectory() as tmp:
        repo = FixtureRepo(Path(tmp))
        repo.write("crates/storage-pg/migrations/0032_previous.sql", "-- old\n")
        repo.write("flavors/example/migrations/20261001000020_old.sql", "-- old\n")
        repo.release("v0.0.35")
        repo.write("crates/storage-pg/migrations/0033_baseline.sql", "-- baseline\n")
        repo.write("crates/storage-pg/definitions/functions.sql", "-- definitions\n")
        diagnostics, _ = check_released_migrations(
            repo.root, epoch="v0.0.36", pending="v0.0.36", env=repo.env
        )
        if diagnostics or check_new_versions(repo.root, repo.env):
            failures.append("pending epoch and a higher new migration should pass")
        repo.release("v0.0.36")
        (repo.root / "crates/storage-pg/migrations/0033_baseline.sql").write_text("-- changed\n")
        diagnostics, _ = check_released_migrations(repo.root, epoch="v0.0.36", env=repo.env)
        if not diagnostics:
            failures.append("editing an epoch migration should fail")
        repo.release("v0.0.37")
        diagnostics, _ = check_released_migrations(repo.root, epoch="v0.0.36", env=repo.env)
        if not any("v0.0.36" in diagnostic for diagnostic in diagnostics):
            failures.append("a later tag must not mask an edited earlier release")
        (repo.root / "crates/storage-pg/migrations/0033_baseline.sql").write_text("-- baseline\n")
        repo.write("crates/storage-pg/migrations/0032_reused.sql", "-- bad\n")
        if not check_new_versions(repo.root, repo.env):
            failures.append("a new version at or below the released maximum should fail")
        (repo.root / "crates/storage-pg/migrations/0032_reused.sql").unlink()
        repo.write("crates/storage-pg/migrations/0034_first.sql", "-- one agent\n")
        if check_unique_versions(repo.root):
            failures.append("one new migration per version should pass")
        repo.write("crates/storage-pg/migrations/0034_second.sql", "-- another agent\n")
        if not check_unique_versions(repo.root):
            failures.append("two new migrations with one version should fail")
        repo.write("flavors/example/migrations/0034_flavor.sql", "-- other directory\n")
        if len(check_unique_versions(repo.root)) != 1:
            failures.append("versions are unique per directory, not across directories")
        repo.write("crates/storage-pg/definitions/functions.sql", "-- changed definitions\n")
        if any(
            "definitions" in diagnostic
            for diagnostic in check_released_migrations(repo.root, epoch="v0.0.36", env=repo.env)[0]
        ):
            failures.append("definitions must not be pinned")
    failures.extend(retired_lane_self_test())
    if failures:
        print("migration self-test failed:", file=sys.stderr)
        for failure in failures:
            print(f"  {failure}", file=sys.stderr)
        return 1
    print("migration self-test OK")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--root", type=Path, default=ROOT)
    args = parser.parse_args()
    return self_test() if args.self_test else run(args.root.resolve())


if __name__ == "__main__":
    raise SystemExit(main())
