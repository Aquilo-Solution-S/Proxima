#!/usr/bin/env python3
"""Validate SQLx migration version reservations, and lock released migrations.

Core and flavors share SQLx's default `_sqlx_migrations` version namespace.
Runtime boot rejects duplicate versions before applying migrations; this check
keeps the documented source lanes honest at review time.

It also locks the *content* of every released migration. SQLx checksums a
migration's bytes: editing a file that live databases have already applied
changes the checksum of an applied version, and `ensure_core_ledger_compatible`
then refuses to boot. The pins are the release tags themselves — every `v*`
tag from RELEASE_EPOCH on must still find its migration files, byte for byte,
at HEAD — so a release pins its own files with nobody keeping a list.
"""
from __future__ import annotations

import argparse
import os
import re
import subprocess
import sys
import tempfile
from dataclasses import dataclass
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
MIGRATION_FILE = re.compile(r"^(?P<version>\d+)_[A-Za-z0-9][A-Za-z0-9_-]*\.sql$")

# The release tag AGENTS.md and docs/how-to/migrations.md both specify:
# `000N_v0XY_<what>.sql` for core, one dated `_v0XY_` file per flavor. Stated
# in prose in two places and enforced in neither, it had already drifted — five
# core files carry no tag, and `0006_v013_` predates `0011_v012_` by ten days,
# so the filename stopped answering "which release shipped this schema change".
RELEASE_TAGGED = re.compile(r"^\d+_v0\d{2}_[A-Za-z0-9][A-Za-z0-9_-]*\.sql$")

# Applied migrations are never renamed: the name is what an operator correlates
# with `_sqlx_migrations`, and the policy above exists precisely to stop files
# moving under live databases. These predate the check and are grandfathered by
# name; the point of the check is that the set cannot grow.
UNTAGGED_GRANDFATHERED = frozenset(
    {
        "crates/storage-pg/migrations/0001_v008.sql",
        "crates/storage-pg/migrations/0002_goal_evidence.sql",
        "crates/storage-pg/migrations/0003_owner_transfer.sql",
        "crates/storage-pg/migrations/0004_cold_object_v4.sql",
        "crates/storage-pg/migrations/0005_erased_pin_targets.sql",
        "crates/storage-pg/migrations/0007_upload_content_identity.sql",
        "crates/storage-pg/migrations/0008_cold_integrity_digest.sql",
        "crates/storage-pg/migrations/0009_declared_sidecar_presence.sql",
        "crates/storage-pg/migrations/0010_purge_queue_backend.sql",
        "flavors/code/migrations/20260818000020_v008_baseline.sql",
        "flavors/code/migrations/20260824000020_v009_declaration_triggers.sql",
        "flavors/code/migrations/20260901000020_declared_sidecar_presence.sql",
    }
)


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


# Where SQLx reads migrations from: the core lane and each flavor's lane. SQLx
# reads one directory level and only `.sql` files, so nothing else is pinned.
RELEASED_MIGRATION = re.compile(
    r"^(?:crates/storage-pg/migrations|flavors/[^/]+/migrations)/[^/]+\.sql$"
)
RELEASE_TAG = re.compile(r"^v(?P<major>\d+)\.(?P<minor>\d+)\.(?P<patch>\d+)$")

# The first release of the current migration lane. v0.0.8 shipped the
# destructive baseline (`0001_v008.sql`, `20260818000020_v008_baseline.sql`)
# and deliberately deleted every file the tags before it shipped
# (docs/how-to/migrations.md rule 3), so those tags pin nothing. A future
# destructive baseline bumps RELEASE_VERSION and moves this to that release in
# the same reviewed change: until the merge cuts its tag, the epoch names the
# pending release and no earlier tag pins anything.
RELEASE_EPOCH = "v0.0.8"

# `release.yml` cuts `v${RELEASE_VERSION}` on merge; the pending release.
RELEASE_VERSION_SOURCE = "crates/core/src/lib.rs"
RELEASE_VERSION_CONST = re.compile(
    r'^pub const RELEASE_VERSION: &str = "(?P<version>\d+\.\d+\.\d+)";$', re.MULTILINE
)

# (tag, file) pairs whose released bytes may differ from HEAD. Closed: it holds
# one entry and only ever shrinks (an entry older than the epoch drops out).
# d12da4f2 ("activate owner RLS without a parameter grant")
# edited 0014 in place after v0.0.15 shipped it; every database that had
# applied the v0.0.15 bytes refused to boot on v0.0.16+ with "core versions
# [14] were amended after this database applied them", and the quality
# deployment was down until its database was reset (2026-09-24). The amended
# bytes are themselves pinned by v0.0.16, so this excuses nothing further.
RELEASED_EDIT_GRANDFATHERED = frozenset(
    {("v0.0.15", "crates/storage-pg/migrations/0014_v015_owner_rls.sql")}
)

RELEASED_REMEDY = (
    "released migrations are immutable; revert the file and ship the fix as a NEW "
    "migration (docs/how-to/migrations.md)"
)


def git(root: Path, *args: str, env: dict[str, str] | None = None, stdin: str | None = None) -> str:
    result = subprocess.run(
        ["git", *args],
        cwd=root,
        env=env,
        input=stdin,
        capture_output=True,
        text=True,
        check=True,
    )
    return result.stdout


def release_tags(root: Path, env: dict[str, str] | None = None) -> list[tuple[tuple[int, int, int], str]]:
    """Every `vMAJOR.MINOR.PATCH` tag, oldest first."""
    tags = []
    for tag in git(root, "tag", "--list", "v*", env=env).split():
        match = RELEASE_TAG.fullmatch(tag)
        if match is not None:
            tags.append(((int(match["major"]), int(match["minor"]), int(match["patch"])), tag))
    return sorted(tags)


def tagged_migrations(root: Path, tag: str, env: dict[str, str] | None = None) -> dict[str, str]:
    """`path -> blob id` of the migration files `tag` shipped (`git ls-tree -r`)."""
    files: dict[str, str] = {}
    for entry in git(root, "ls-tree", "-r", "-z", "--full-tree", tag, env=env).split("\0"):
        if not entry:
            continue
        meta, path = entry.split("\t", 1)
        _mode, kind, blob = meta.split()
        if kind == "blob" and RELEASED_MIGRATION.fullmatch(path):
            files[path] = blob
    return files


def head_blobs(root: Path, paths: list[str], env: dict[str, str] | None = None) -> dict[str, str | None]:
    """`path -> blob id` of each file as it stands now (`git hash-object`), None if deleted."""
    present = [path for path in paths if (root / path).is_file()]
    blobs: dict[str, str | None] = dict.fromkeys(paths)
    if present:
        hashed = git(root, "hash-object", "--stdin-paths", env=env, stdin="\n".join(present) + "\n")
        blobs.update(zip(present, hashed.split(), strict=True))
    return blobs


def pending_release(root: Path) -> str | None:
    """`v{RELEASE_VERSION}`: the tag merging this tree cuts (or already cut)."""
    try:
        found = RELEASE_VERSION_CONST.search((root / RELEASE_VERSION_SOURCE).read_text(encoding="utf-8"))
    except OSError:
        return None
    return f"v{found['version']}" if found else None


def check_released_migrations(
    root: Path,
    *,
    epoch: str = RELEASE_EPOCH,
    grandfathered: frozenset[tuple[str, str]] = RELEASED_EDIT_GRANDFATHERED,
    pending: str | None = None,
    env: dict[str, str] | None = None,
) -> tuple[list[str], str]:
    """Every migration file a release tag from `epoch` on shipped must be unchanged at HEAD.

    Fails closed: a clone without the tags (or their trees) cannot prove
    anything, so it fails rather than passing vacuously. The one exception is
    an epoch naming the pending release (`pending`, default
    `v{RELEASE_VERSION}`) newer than every tag present: the destructive
    baseline that release cuts, whose tag only exists after the merge.
    """
    try:
        tags = release_tags(root, env)
    except (OSError, subprocess.CalledProcessError) as error:
        return [f"cannot list release tags ({error}); run from a git checkout"], ""
    epoch_match = RELEASE_TAG.fullmatch(epoch)
    if epoch_match is None:
        return [f"release epoch {epoch!r} is not a vMAJOR.MINOR.PATCH tag"], ""
    floor = (int(epoch_match["major"]), int(epoch_match["minor"]), int(epoch_match["patch"]))
    if epoch not in {tag for _, tag in tags}:
        pending = pending_release(root) if pending is None else pending
        if tags and epoch == pending and floor > tags[-1][0]:
            return [], (
                f"released migrations: RELEASE_EPOCH {epoch} is the release this change cuts; "
                f"no earlier tag (latest {tags[-1][1]}) pins anything"
            )
        return [
            f"release tag {epoch} is not in this clone; fetch tags "
            "(actions/checkout `fetch-tags: true`, or `git fetch --tags`) — "
            "without them no released migration is checked"
        ], ""

    diagnostics: list[str] = []
    shipped: dict[tuple[str, str], list[str]] = {}
    checked = [tag for version, tag in tags if version >= floor]
    for tag in checked:
        try:
            files = tagged_migrations(root, tag, env)
        except subprocess.CalledProcessError as error:
            diagnostics.append(f"{tag}: cannot read the tag's tree ({error.stderr.strip()}); fetch tags")
            continue
        for path, blob in files.items():
            shipped.setdefault((path, blob), []).append(tag)

    paths = sorted({path for path, _ in shipped})
    now = head_blobs(root, paths, env)
    excused: set[tuple[str, str]] = set()
    order = {tag: index for index, tag in enumerate(checked)}
    for (path, blob), in_tags in sorted(shipped.items(), key=lambda item: (item[0][0], order[item[1][0]])):
        if now[path] == blob:
            continue
        pinned_by = [tag for tag in in_tags if (tag, path) not in grandfathered]
        excused.update((tag, path) for tag in in_tags if (tag, path) in grandfathered)
        if not pinned_by:
            continue
        change = "deleted" if now[path] is None else f"changed (HEAD blob {now[path]})"
        diagnostics.append(
            f"{path}: released as blob {blob} in {', '.join(pinned_by)}, now {change} — "
            f"{RELEASED_REMEDY}"
        )
    # An entry that excuses nothing is a pre-authorised edit waiting to happen.
    # One older than the epoch pins nothing either way and simply drops out.
    for tag, path in sorted(grandfathered - excused):
        tag_match = RELEASE_TAG.fullmatch(tag)
        if tag_match and (int(tag_match["major"]), int(tag_match["minor"]), int(tag_match["patch"])) < floor:
            continue
        diagnostics.append(
            f"grandfathered ({tag}, {path}) excuses no difference — remove it; "
            "the list only shrinks"
        )
    summary = (
        f"released migrations unchanged: {len(paths)} files across {len(checked)} tags "
        f"({checked[0]}..{checked[-1]})"
        if checked
        else ""
    )
    return diagnostics, summary


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
            if not RELEASE_TAGGED.fullmatch(path.name) and str(rel) not in UNTAGGED_GRANDFATHERED:
                diagnostics.append(
                    f"{rel}: migration filename must name its release, "
                    f"<version>_v0XY_<description>.sql (AGENTS.md, docs/how-to/migrations.md)"
                )
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
    released, summary = check_released_migrations(root)
    diagnostics = validate(root) + released
    if diagnostics:
        print("migration range check failed:", file=sys.stderr)
        for diagnostic in diagnostics:
            print(f"  {diagnostic}", file=sys.stderr)
        return 1
    versions, _ = collect(root)
    rendered = ", ".join(f"{item.source}:{item.version}" for item in sorted(versions, key=lambda v: v.version))
    print(f"migration range check OK: {rendered}")
    print(summary)
    for tag, path in sorted(RELEASED_EDIT_GRANDFATHERED):
        print(f"grandfathered released edit: {tag} {path}")
    return 0


def write_fixture(root: Path, files: dict[str, list[str]]) -> None:
    for rel_dir, names in files.items():
        directory = root / rel_dir
        directory.mkdir(parents=True)
        for name in names:
            (directory / name).write_text("-- fixture\n", encoding="utf-8")


FIXTURE_CORE = "crates/storage-pg/migrations"
FIXTURE_FLAVOR = "flavors/code/migrations"


class FixtureRepo:
    """A throwaway git repository whose tags stand in for releases."""

    def __init__(self, root: Path) -> None:
        self.root = root
        # Never inherit a hook's GIT_DIR/GIT_INDEX_FILE, or the fixture would
        # write into the real repository.
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

    def write(self, rel: str, content: str) -> None:
        path = self.root / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content, encoding="utf-8")

    def release(self, tag: str) -> None:
        self.git("add", "--all")
        self.git("commit", "-q", "--allow-empty", "-m", tag)
        # Annotated, like the real release tags.
        self.git("tag", "-a", tag, "-m", tag)

    def check(
        self,
        grandfathered: frozenset[tuple[str, str]] = frozenset(),
        epoch: str = "v0.0.8",
        pending: str = "v0.0.9",
    ) -> list[str]:
        diagnostics, _ = check_released_migrations(
            self.root, epoch=epoch, grandfathered=grandfathered, pending=pending, env=self.env
        )
        return diagnostics


def released_fixture(root: Path) -> FixtureRepo:
    """v0.0.7 ships a lane v0.0.8 deletes; v0.0.8 and v0.0.9 ship the current one."""
    repo = FixtureRepo(root)
    repo.write(f"{FIXTURE_CORE}/0001_init.sql", "-- pre-epoch\n")
    repo.release("v0.0.7")
    (root / FIXTURE_CORE / "0001_init.sql").unlink()
    repo.write(f"{FIXTURE_CORE}/0001_v008.sql", "-- baseline\n")
    repo.write(f"{FIXTURE_FLAVOR}/20260818000020_v008_baseline.sql", "-- flavor baseline\n")
    repo.write(f"{FIXTURE_FLAVOR}/README.md", "not a migration\n")
    repo.release("v0.0.8")
    repo.write(f"{FIXTURE_CORE}/0002_v009_thing.sql", "-- v009\n")
    repo.release("v0.0.9")
    return repo


def destructive_baseline(repo: FixtureRepo) -> None:
    """Replace the v0.0.8 lane with a new baseline, as a destructive release does."""
    for rel in (
        f"{FIXTURE_CORE}/0001_v008.sql",
        f"{FIXTURE_CORE}/0002_v009_thing.sql",
        f"{FIXTURE_FLAVOR}/20260818000020_v008_baseline.sql",
    ):
        (repo.root / rel).unlink()
    repo.write(f"{FIXTURE_CORE}/0003_v010.sql", "-- new baseline\n")
    repo.write(f"{FIXTURE_FLAVOR}/20261001000020_v010_baseline.sql", "-- new flavor baseline\n")


def released_self_test() -> list[str]:
    """Prove the released-migration lock against real tags in a temp repository."""
    edited_0002 = ("v0.0.9", f"{FIXTURE_CORE}/0002_v009_thing.sql")
    # A destructive baseline: RELEASE_VERSION -> 0.0.10 and RELEASE_EPOCH ->
    # v0.0.10 in the same change, before the merge cuts v0.0.10.
    epoch_cases: list[tuple[str, object, str, str, frozenset[tuple[str, str]], bool]] = [
        ("a destructive baseline at the pending release passes", destructive_baseline,
         "v0.0.10", "v0.0.10", frozenset(), False),
        ("after the merge cuts it, a grandfather older than the epoch drops out",
         lambda repo: (destructive_baseline(repo), repo.release("v0.0.10")),
         "v0.0.10", "v0.0.10", frozenset({edited_0002}), False),
        ("the same baseline without moving the epoch fails", destructive_baseline,
         "v0.0.8", "v0.0.10", frozenset(), True),
        ("an epoch naming a release that is not pending fails closed", destructive_baseline,
         "v0.0.10", "v0.0.9", frozenset(), True),
        ("a pending epoch older than an existing tag fails closed",
         lambda repo: (destructive_baseline(repo), repo.release("v0.0.11")),
         "v0.0.10", "v0.0.10", frozenset(), True),
        ("a pending epoch in a clone without tags fails closed",
         lambda repo: [repo.git("tag", "-d", tag) for tag in ("v0.0.7", "v0.0.8", "v0.0.9")],
         "v0.0.10", "v0.0.10", frozenset(), True),
    ]
    cases: list[tuple[str, object, frozenset[tuple[str, str]], bool]] = [
        ("unchanged release passes", lambda repo: None, frozenset(), False),
        (
            "a new migration passes",
            lambda repo: repo.write(f"{FIXTURE_CORE}/0003_v010_next.sql", "-- new\n"),
            frozenset(),
            False,
        ),
        (
            "a committed new migration passes",
            lambda repo: (
                repo.write(f"{FIXTURE_CORE}/0003_v010_next.sql", "-- new\n"),
                repo.git("add", "--all"),
                repo.git("commit", "-q", "-m", "next"),
            ),
            frozenset(),
            False,
        ),
        (
            "editing a tagged migration fails",
            lambda repo: repo.write(f"{FIXTURE_CORE}/0001_v008.sql", "-- baseline\n-- edit\n"),
            frozenset(),
            True,
        ),
        (
            "a committed edit to a tagged migration fails",
            lambda repo: (
                repo.write(f"{FIXTURE_CORE}/0002_v009_thing.sql", "-- v009 amended\n"),
                repo.git("commit", "-q", "-am", "amend"),
            ),
            frozenset(),
            True,
        ),
        (
            "editing a tagged flavor migration fails",
            lambda repo: repo.write(f"{FIXTURE_FLAVOR}/20260818000020_v008_baseline.sql", "--\n"),
            frozenset(),
            True,
        ),
        (
            "deleting a tagged migration fails",
            lambda repo: (repo.root / FIXTURE_CORE / "0002_v009_thing.sql").unlink(),
            frozenset(),
            True,
        ),
        (
            "a non-.sql file beside the migrations is not pinned",
            lambda repo: repo.write(f"{FIXTURE_FLAVOR}/README.md", "changed\n"),
            frozenset(),
            False,
        ),
        (
            "a grandfathered edit passes once a later tag pins the new bytes",
            lambda repo: (
                repo.write(f"{FIXTURE_CORE}/0002_v009_thing.sql", "-- v009 amended\n"),
                repo.release("v0.0.10"),
            ),
            frozenset({edited_0002}),
            False,
        ),
        (
            "the same edit without the grandfather fails",
            lambda repo: (
                repo.write(f"{FIXTURE_CORE}/0002_v009_thing.sql", "-- v009 amended\n"),
                repo.release("v0.0.10"),
            ),
            frozenset(),
            True,
        ),
        (
            "a grandfather does not excuse a second edit",
            lambda repo: (
                repo.write(f"{FIXTURE_CORE}/0002_v009_thing.sql", "-- v009 amended\n"),
                repo.release("v0.0.10"),
                repo.write(f"{FIXTURE_CORE}/0002_v009_thing.sql", "-- v009 amended twice\n"),
            ),
            frozenset({edited_0002}),
            True,
        ),
        (
            "a grandfather that excuses nothing fails",
            lambda repo: None,
            frozenset({edited_0002}),
            True,
        ),
        (
            "tags before the epoch pin nothing",
            lambda repo: None,
            frozenset(),
            False,
        ),
        (
            "a clone without the epoch tag fails closed",
            lambda repo: repo.git("tag", "-d", "v0.0.8"),
            frozenset(),
            True,
        ),
    ]
    failures: list[str] = []
    for name, mutate, grandfathered, should_fail in cases:
        with tempfile.TemporaryDirectory() as tmp:
            repo = released_fixture(Path(tmp))
            mutate(repo)
            diagnostics = repo.check(grandfathered)
            if bool(diagnostics) != should_fail:
                failures.append(f"released: {name}: expected fail={should_fail}, got {diagnostics}")
    for name, mutate, epoch, pending, grandfathered, should_fail in epoch_cases:
        with tempfile.TemporaryDirectory() as tmp:
            repo = released_fixture(Path(tmp))
            mutate(repo)
            diagnostics = repo.check(grandfathered, epoch=epoch, pending=pending)
            if bool(diagnostics) != should_fail:
                failures.append(f"released: {name}: expected fail={should_fail}, got {diagnostics}")
    with tempfile.TemporaryDirectory() as tmp:
        repo = FixtureRepo(Path(tmp))
        repo.write(f"{FIXTURE_CORE}/0001_v008.sql", "-- baseline\n")
        repo.git("add", "--all")
        repo.git("commit", "-q", "-m", "untagged")
        if not repo.check():
            failures.append("released: a clone with no tags at all: expected fail=True")
    if not RELEASED_EDIT_GRANDFATHERED <= {
        ("v0.0.15", "crates/storage-pg/migrations/0014_v015_owner_rls.sql")
    }:
        failures.append(
            "released: RELEASED_EDIT_GRANDFATHERED only shrinks from its one v0.0.15 entry "
            "(docs/how-to/migrations.md rule 2)"
        )
    if pending_release(ROOT) is None:
        failures.append(f"released: no RELEASE_VERSION in {RELEASE_VERSION_SOURCE}")
    return failures


def self_test() -> int:
    cases = [
        (
            "current lanes accept disjoint versions",
            {
                "crates/storage-pg/migrations": ["0001_v008_init.sql", "0008_v005_thing.sql"],
                "flavors/code/migrations": ["20260801000020_v007_baseline.sql"],
            },
            False,
        ),
        (
            "duplicate versions fail",
            {
                "crates/storage-pg/migrations": ["0001_v008_init.sql"],
                "flavors/code/migrations": [
                    "20260801000020_v007_a.sql",
                    "20260801000020_v007_b.sql",
                ],
            },
            True,
        ),
        (
            "wrong suffix lane fails",
            {
                "crates/storage-pg/migrations": ["0001_v008_init.sql"],
                "flavors/code/migrations": [
                    "20260612000010_v007_baseline.sql",
                    "20260801000020_v007_baseline.sql",
                ],
            },
            True,
        ),
        (
            "untagged core migration fails",
            {
                "crates/storage-pg/migrations": ["0001_v008_init.sql", "0013_purge_queue.sql"],
                "flavors/code/migrations": ["20260801000020_v007_baseline.sql"],
            },
            True,
        ),
        (
            "untagged flavor migration fails",
            {
                "crates/storage-pg/migrations": ["0001_v008_init.sql"],
                "flavors/code/migrations": ["20260801000020_baseline.sql"],
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

    failures.extend(released_self_test())

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
