# Migration policy

How schema migrations are authored.

Core is one file: `crates/storage-pg/migrations/0001_v008.sql`.
Flavor: `flavors/code/migrations/20260818000020_v008_baseline.sql`.
A database whose ledger does not match those files must reset.

## Rules

1. **A version number is never reused** on a database you do not personally own.
   Replacement takes a new version. Gaps are normal.

2. **One frozen file per release.** Drafts during a cycle; squash into one
   `NNNN_vX.Y.Z.sql` under a fresh version before tag. After the tag, that
   file is frozen.

3. **The migrations directory is the schema**, not a changelog. Replay
   `0001_v008.sql` on an empty DB to see the shape.

## sqlx ledger

`_sqlx_migrations` stores `(version, checksum)`.

- Orphan row (version the binary does not embed): forgiven (`ignore_missing`).
- Checksum mismatch: fatal. Do not edit an applied file.

## Lanes

Core versions are small integers (`0001_v008.sql`). Flavor versions are
date-shaped. Boundary: `CORE_MIGRATION_VERSION_CEILING` (9999).

Core ledger: `public._sqlx_migrations`.
Code flavor: `public._sqlx_migrations_proxima_code`.

## Cycle

- During: add draft files. Amend only if no shared DB applied them.
- At tag: squash to one new version; delete drafts; `ensure_core_schema_markers`
  matches the lane.
- Fresh install: apply the one file.
- Dev DB that ran an older checksum of version 1: reset.

## Tooling

No retired-version lists in code. Boot floor is `min_core_migration_version()`
(newest embedded core file). `dev-migrate --stamp` requires
`ensure_core_schema_markers`. Otherwise reset.
