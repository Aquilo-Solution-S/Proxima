# Migration policy

How schema migrations are authored.

The frozen baselines are `crates/storage-pg/migrations/0001_v008.sql` and
`flavors/code/migrations/20260818000020_v008_baseline.sql`. A database whose
ledger does not match those files must reset.

**From v0.0.9 on, releases are additive.** A frozen baseline is never edited,
existing databases upgrade in place, and a release that needs schema work
ships **exactly one migration file per version** — `000N_v0XY_<what>.sql`
for core, one dated `_v0XY_` file per flavor — never several, never edited
after the tag. v0.0.9 is `0002_v009_declaration_triggers.sql` (core) and
`20260824000020_v009_declaration_triggers.sql` (code flavor).

## v0.0.15

| Lane | Migration |
|---|---|
| Core | `0013_v015_agent_note_natural_key_index.sql`: btree on `proxima_core.agent_note_v1 (note_id)`, the natural key `core/agent-note-v1` declares |
| Code flavor | No new migration |

Index-only and additive. A v0.0.14 database upgrades in place; no reset.
`CREATE INDEX` takes a brief `SHARE` lock on `agent_note_v1`, which blocks
writes to that one table for the duration — trivial on any store small enough
to have tolerated the missing index, and the reason for the index is that it
was not staying small.

### Owner-RLS compatibility bridge

v0.0.15 binds verified owner/platform scopes before enforcement. It recognizes
one exact successor checksum from
`crates/storage-pg/compatibility/0014_v016_owner_rls.sql` without applying that
file. Unknown versions and changed checksums still refuse boot. The Code
companion is `flavors/code/compatibility/20260922000020_v016_owner_rls.sql`.
The prepared core migration also adds the two owner/head ordering indexes
used by paginated reads (see [07 §Runtime owner binding](../07-storage.md#runtime-owner-binding)).

Activation requires every live host sharing the database to run the bridge,
separate runtime/platform credentials, and both additive migrations. Promote
these exact bytes into the migration directories for v0.0.16; changing them
requires a new compatible predecessor. See [15 §Owner-RLS rollout](../15-deployment.md#owner-rls-rollout).

## v0.0.14

No core or flavor migration ships in this release. Existing v0.0.13 databases
remain compatible and require no reset.

## v0.0.13

No core or flavor migration ships in this release. Existing v0.0.12 databases
remain compatible and require no reset.

## v0.0.12

| Lane | Migration |
|---|---|
| Core | `0011_v012_fact_outbox.sql`: publication state enum, transactional Fact outbox, claim index, immutable-envelope guards |
| Code flavor | No new migration |

Normal facade boot applies pending migrations automatically. A compatible
v0.0.11 database upgrades in place; fresh installs replay the embedded files
in order. Previously shipped migration bytes remain unchanged.

## Rules

1. **A version number is never reused** on a database you do not personally own.
   Replacement takes a new version. Gaps are normal.

2. **A frozen baseline is never edited.** SQLx checksums a migration's bytes,
   so editing an applied file changes the checksum of a version live databases
   have already recorded — `ensure_core_ledger_compatible` then answers
   `SchemaResetRequired`, a destructive reset with no schema reason behind it.
   Add a new additive migration instead. `scripts/check-migration-ranges.py`
   content-pins both baselines and fails the build on an edited byte.

3. **A new baseline is a release decision, not a side effect.** Replacing a
   baseline resets every deployed database, so it happens only for a
   deliberately named destructive release — a new file under a new version,
   pinned like the ones before it. Drafts during a cycle still squash under a
   fresh version before tag; they just append rather than replace.

4. **The migrations directory is the schema**, not a changelog. Replay the
   directory in version order on an empty DB to see the shape.

## sqlx ledger

`_sqlx_migrations` stores `(version, checksum)`.

- SQLx ignores foreign-lane rows (`ignore_missing`). Core preflight still refuses
  unknown core versions except the checksum-approved owner-RLS successor.
- Checksum mismatch: fatal. Do not edit an applied file.

## Lanes

Core versions are small integers (`0001_v008.sql`). Flavor versions are
date-shaped. Boundary: `CORE_MIGRATION_VERSION_CEILING` (9999).

Core ledger: `public._sqlx_migrations`.
Code flavor: `public._sqlx_migrations_proxima_code`.

## Cycle

- During: add draft files. Amend only if no shared DB applied them.
- At tag: squash the cycle's drafts to one new version appended after the
  frozen baseline; delete the drafts; `ensure_core_schema_markers` matches the
  lane.
- Fresh install: apply the directory in version order.
- Existing install: apply the pending versions; the baseline is not re-run.
- Dev DB that ran an older checksum of version 1: reset.

## Tooling

No retired-version lists in code. Boot floor is `min_core_migration_version()`
(newest embedded core file). `dev-migrate --stamp` requires
`ensure_core_schema_markers`. Otherwise reset.
