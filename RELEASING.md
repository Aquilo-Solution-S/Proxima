# Releasing Proxima

Releases are identified by **git tag**. Crate versions in `Cargo.toml` stay at
`0.1.0`: every crate is `publish = false`. The boot floor is derived from the
embedded migration set (`min_core_migration_version()`).

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
`ensure_core_schema_markers` in `crates/storage-pg/src/lib.rs`.

The boot floor needs no bump: it is derived from the embedded migration set
(`min_core_migration_version()`), so shipping the squashed file is what
raises it. The markers are what stop a `PROXIMA_SKIP_MIGRATIONS` boot from
starting green against a database one lane behind and then failing every
query.

**3. Confirm the schema file list.**

```sh
ls crates/storage-pg/migrations/ flavors/*/migrations/
```

v0.0.8 core is `0001_v008.sql`. Flavor is one baseline file. Existing
databases reset; there is no in-place ALTER lane.

**4. Document breaking changes in `CHANGELOG.md`** (git-cliff on the tag).
`git log --oneline "$(git describe --tags --abbrev=0)..HEAD" | grep '!'`
lists the `!` commits. Struct field additions to non-`#[non_exhaustive]`
public types count.

**5. Run the full gate.**

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets
PROXIMA_TEST_PG_URL=postgres://... cargo test --workspace --no-fail-fast
# Code flavor is the proxima-mcp default, so --workspace covers the OIDC
# e2e (served Code-flavor tool list). REST still needs --features rest.
PROXIMA_TEST_PG_URL=postgres://... cargo test -p proxima-mcp --features rest
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
