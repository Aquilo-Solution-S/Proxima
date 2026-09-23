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

## v0.0.16

| Lane | Migration |
|---|---|
| Core | `0015_v016_embedding_spaces.sql`: `embeddings.vec` becomes untyped `vector` with a `dim` column on embeddings, heads and jobs; one partial HNSW index per supported width replaces `idx_embeddings_vec_hnsw` |

Requires pgvector `>= 0.8.0`; the migration refuses an older extension, and
boot checks it again. Existing rows are 1024 wide and are labelled so without
a table rewrite, but the 1024 index is rebuilt over every existing vector
during the migration — budget the same time the original index build took.

The cold-archive record format moves to v8, which stores each vector's width.
v0.0.16 reads older records as 1024 wide; an older binary cannot read a v8
record, so a rollback after a Fact has been cooled needs the database restored
from before the upgrade.

`maintain-embeddings --drain` is removed; the serving process drains the jobs
maintenance enqueues. Remove `--drain` from any scheduled invocation.

## v0.0.15

| Lane | Migration |
|---|---|
| Core | Existing `0013_v015_agent_note_natural_key_index.sql`, unchanged; new `0014_v015_owner_rls.sql` activates owner RLS and adds covering head-page indexes |
| Code flavor | `20260922000020_v015_owner_rls.sql`: owner policies and FORCE RLS on every Code table |

**Coordinated cutover, not a rolling upgrade from v0.0.14.** Stop every old
pack sharing the database before applying migrations; old binaries cannot
resume after activation. Schema/data changes are additive; owner-scoped access
and split runtime/platform credentials become mandatory.

The already-merged 0013 may have been applied independently. Its bytes and
version remain unchanged. v0.0.15 is the named exception to the one-core-file
rule: append 0014 instead of rewriting an existing checksum. Both core and
Code activation now run through the ordinary composed migration runner in
this release. There is no future-version checksum exception or second release
needed for activation.

Migration authority is the nonsuperuser platform owner, with an administrator
preparing extensions, ownership and grants first. See
[15 §Owner-RLS rollout](../15-deployment.md#owner-rls-rollout).

0014 was corrected in place after the v0.0.15 tag: the tagged file refused to
run without a superuser-issued `GRANT SET ON PARAMETER app.proxima_scope`, so a
database without that grant never recorded it. A database that did record the
tagged 0014 fails the ledger checksum check at boot; restore it to before the
cutover and re-run the migration.

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

- SQLx ignores foreign-lane rows (`ignore_missing`). Core preflight requires
  every recorded core version to match an embedded migration checksum.
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
