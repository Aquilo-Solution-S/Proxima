# Releasing Proxima

The procedure that produced v0.0.4 through v0.0.6, written down. Before this
file it lived only in the maintainer's head, which is why the v0.0.7 audit
found the boot-floor constant still pointing at the previous release —
nothing prompted anyone to bump it. (That constant is gone: the floor is now
derived from the embedded migration set.)

Releases are identified by **git tag**. Crate versions in `Cargo.toml` stay at
`0.1.0`: every crate is `publish = false`, so those numbers carry no meaning
and bumping thirteen manifests each release only invites drift.

## Before the tag

**1. Bump the release version.** `proxima_core::RELEASE_VERSION` in
`crates/core/src/lib.rs`. This is what MCP clients see in
`initialize.serverInfo.version` and the only in-code statement of which
release a deployment is running.

**2. Squash the cycle's drafts and refresh the lane's markers.** Fold the
release's draft migration files into one `NNNN_vX.Y.Z.sql` under a **fresh,
never-used version number** — never by editing a file any shared database has
already applied ([docs/how-to/migrations.md](docs/how-to/migrations.md) has
the rules and the why; reusing a version number is the one unrecoverable
mistake). Then add the release's structural markers to
`ensure_core_schema_markers` in `crates/storage-pg/src/lib.rs`, and
regenerate the schema artifact:

```sh
scripts/regen-schema-sql.sh    # commit the db/schema.*.sql diffs
```

The boot floor needs no bump: it is derived from the embedded migration set
(`min_core_migration_version()`), so shipping the squashed file is what
raises it. The markers are what stop a `PROXIMA_SKIP_MIGRATIONS` boot from
starting green against a database one lane behind and then failing every
query.

**3. Write the schema lane into `MIGRATING.md`.** One `## The vX.Y.Z schema
lane` section per release that ships a migration, listing **every** file by
source (core and each flavor), and stating explicitly:

- whether it is online-safe, and if not, the measured lock window;
- any behaviour change that does not announce itself (a changed primary key,
  a changed text-search config, a changed default);
- the rollback position.

An operator promoting through GitOps applies exactly what this section lists.
If a migration is not named here, it does not get applied. Check the list
against the directories, not against memory — the v0.0.7 lane named 2 of its
7 files for most of the cycle, because each new migration was described in the
prose of the section that introduced it and nobody went back to the table:

```sh
ls crates/storage-pg/migrations/ flavors/*/migrations/
```

**4. Document every breaking change**, one `MIGRATING.md` entry per `!`-marked
commit as it lands:

```sh
git log --oneline "$(git describe --tags --abbrev=0)..HEAD" | grep '!'
```

Struct field additions to non-`#[non_exhaustive]` public types count — they
break out-of-tree struct literals at compile time.

**4b. Consolidate the release's entries before the tag.** Step 4 appends in
commit order, which is right during a cycle and wrong at a tag: v0.0.7
accumulated 43 sections in landing order, half of them "no action required",
and a reader upgrading had to read all 2,167 lines to find the eight things
that applied to them. Before tagging, fold the release into the shape the rest
of the file uses — schema lane, then breaking changes grouped by **audience**
(MCP client, Rust host, flavor author, operator), then behaviour changes that
need verifying, then additive surface in a table.

Keep headings descriptive and **never number them**. Numbered sections make
every PR conflict with the next at the same append point, and a
conflict resolved in the wrong order leaves §55 sitting before §54 — which is
exactly what happened. Cross-reference by anchor link, so
`scripts/check-doc-links.py MIGRATING.md` catches a reference that goes stale.

**5. Run the full gate.**

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets
PROXIMA_TEST_PG_URL=postgres://... cargo test --workspace --no-fail-fast
# proxima-mcp's OIDC e2e suite is #![cfg(feature = "code")], so
# --workspace does not build it. It is the only place the served
# Code-flavor tool list is asserted end to end; skipping it means
# adding or removing a flavor tool goes green locally and red in CI.
PROXIMA_TEST_PG_URL=postgres://... cargo test -p proxima-mcp --features code
python3 scripts/check-sql-policy.py
python3 scripts/check-migration-ranges.py
python3 scripts/check-architecture-guardrails.py
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features --locked
```

**6. Verify the docs still describe a working system.** At minimum, follow
`docs/getting-started/local-dev.md` end to end on a clean database and confirm
an MCP client can connect. Doc rot is invisible to CI.

## Cutting the tag

**7. Regenerate the changelog, stamped with the version being cut.**

```sh
scripts/changelog.sh v0.0.7
```

`CHANGELOG.md` is git-cliff output from Conventional Commit messages — never
hand-edit it. Passing the tag matters: without it, this release's commits land
under "unreleased" because no commit points at the tag yet.

**8. Land the changelog through a PR, then tag the merged commit.**
Branch protection declines direct pushes to `main` — the v0.0.7 ceremony
learned this the hard way: `git push origin main --follow-tags` pushed the
tag but not the commit it pointed at, leaving a tag referencing a commit off
`main` (and a phantom draft release) that had to be deleted and redone.

```sh
git checkout -b docs/changelog-vX.Y.Z
git add CHANGELOG.md
git commit -m "docs(changelog): stamp vX.Y.Z"
git push -u origin docs/changelog-vX.Y.Z
gh pr create --base main --title "docs(changelog): stamp vX.Y.Z" --body "RELEASING.md step 7/8"
# merge once checks are green, then tag the MERGED commit on main:
git fetch origin
git tag -a vX.Y.Z -m "vX.Y.Z" origin/main
git push origin vX.Y.Z
```

`.github/workflows/release.yml` generates the per-tag release notes
independently from the pushed tag.

## After the tag

- Confirm `initialize.serverInfo` on a deployed build reports the new version.
- Confirm the release workflow published notes for the tag.
