# Releasing Proxima

The procedure that produced v0.0.4 through v0.0.6, written down. Before this
file it lived only in the maintainer's head, which is why the v0.0.7 audit
found `MIN_CORE_MIGRATION_VERSION` still pointing at the previous release —
nothing prompted anyone to bump it.

Releases are identified by **git tag**. Crate versions in `Cargo.toml` stay at
`0.1.0`: every crate is `publish = false`, so those numbers carry no meaning
and bumping thirteen manifests each release only invites drift.

## Before the tag

**1. Bump the release version.** `proxima_core::RELEASE_VERSION` in
`crates/core/src/lib.rs`. This is what MCP clients see in
`initialize.serverInfo.version` and the only in-code statement of which
release a deployment is running.

**2. Bump `MIN_CORE_MIGRATION_VERSION`** in `crates/storage-pg/src/lib.rs` if
this release adds a core migration, and add the release's structural markers
to `ensure_core_schema_current`. This is what stops a `PROXIMA_SKIP_MIGRATIONS`
boot from starting green against a database one lane behind and then failing
every query. Verify:

```sh
rg -n 'MIN_CORE_MIGRATION_VERSION' crates/storage-pg/src/lib.rs
ls crates/storage-pg/migrations/    # highest version must match
```

**3. Write the schema lane into `MIGRATING.md`.** One `### vX.Y.Z schema lane`
section per release that ships a migration, listing every file by lane (core
and each flavor), and stating explicitly:

- whether it is online-safe, and if not, the measured lock window;
- any behaviour change that does not announce itself (a changed primary key,
  a changed text-search config, a changed default);
- the rollback position.

An operator promoting through GitOps applies exactly what this section lists.
If a migration is not named here, it does not get applied.

**4. Document every breaking change.** One numbered `MIGRATING.md` section per
`!`-marked commit:

```sh
git log --oneline "$(git describe --tags --abbrev=0)..HEAD" | grep '!'
```

Struct field additions to non-`#[non_exhaustive]` public types count — they
break out-of-tree struct literals at compile time.

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

**8. Commit and tag.**

```sh
git add CHANGELOG.md
git commit -m "docs(changelog): stamp v0.0.7"
git tag -a v0.0.7 -m "v0.0.7"
git push origin main --follow-tags
```

`.github/workflows/release.yml` generates the per-tag release notes
independently from the pushed tag.

## After the tag

- Confirm `initialize.serverInfo` on a deployed build reports the new version.
- Confirm the release workflow published notes for the tag.
