# Migration policy

How schema migrations are authored.

The frozen baselines are `crates/storage-pg/migrations/0001_v008.sql` and
`flavors/code/migrations/20260818000020_v008_baseline.sql`. A database whose
ledger does not match those files must reset.

**From v0.0.9 on, releases are additive.** A released migration is never
edited (rule 2), existing databases upgrade in place, and a release that needs
schema work ships **exactly one migration file per version** —
`000N_v0XY_<what>.sql` for core, one dated `_v0XY_` file per flavor — never
several. v0.0.9 is `0002_v009_declaration_triggers.sql` (core) and
`20260824000020_v009_declaration_triggers.sql` (code flavor).

## v0.0.16

| Lane | Migration |
|---|---|
| Core | `0015_v016_embedding_spaces.sql`: `embeddings.vec` becomes untyped `vector` with a `dim` column on embeddings, heads and jobs; one partial HNSW index per supported width replaces `idx_embeddings_vec_hnsw` |
| Core | `0016_v016_embedding_claim_order.sql`: the pending-claim index keys on queue position alone, because the drain claims across every embedding space |
| Core | `0017_v016_metadata_write_scope.sql`: owner scope may only insert a lexical language; the lexical default and the flavor surface registry are platform-only |

v0.0.16 is the second named exception to the one-core-file rule: 0015 merged
before per-Owner embedding routing and may have been applied independently,
so its bytes stay and 0016 appends the claim-index change; 0017 appends the
metadata policy fix on the same terms.

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

0014 was edited in place after the v0.0.15 tag (d12da4f2, shipped in v0.0.16).
The tagged file ran only where a superuser had issued
`GRANT SET ON PARAMETER app.proxima_scope`; every database that did record it
refused to boot on v0.0.16+ with `core versions [14] were amended after this
database applied them`, and the quality deployment was down until its database
was reset (2026-09-24). Restore such a database to before the cutover and
re-run the migration. The edit is the single grandfathered entry in
`scripts/check-migration-ranges.py`; rule 2 has no exceptions.

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

2. **A released migration is never edited.** Every migration file a `v*` tag
   shipped is immutable — baseline or not, no exceptions. SQLx checksums a
   migration's bytes, so editing a released file changes the checksum of a
   version live databases have already recorded, and
   `ensure_core_ledger_compatible` refuses to boot them. A fix, including a fix
   to a released migration, ships as a **new** migration.
   `scripts/check-migration-ranges.py` derives the pins from the tags: for each
   `v*` tag from `v0.0.8` on it lists the tag's migration files
   (`git ls-tree -r <tag>`) and fails CI when a file is changed or deleted at
   HEAD (`git hash-object`); new files pass. Tags before `v0.0.8` shipped the
   lane that baseline replaced. The one grandfathered entry, (v0.0.15, 0014),
   predates the check; the list only shrinks.

3. **A new baseline is a release decision, not a side effect.** Replacing a
   baseline resets every deployed database, so it happens only for a
   deliberately named destructive release — a new file under a new version.
   The same change bumps `RELEASE_VERSION` and moves the check's
   `RELEASE_EPOCH` to that release: until the merge cuts its tag no earlier
   tag pins anything, and grandfathered entries older than the epoch drop out.
   Drafts during a cycle still squash under a fresh version before tag; they
   just append rather than replace.

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

- During: add draft files. Amend only if no shared DB applied them; never
  after the tag (rule 2).
- At tag: squash the cycle's drafts to one new version appended after the
  frozen baseline; delete the drafts; `ensure_core_schema_markers` matches the
  lane.
- Fresh install: apply the directory in version order.
- Existing install: apply the pending versions; the baseline is not re-run.
- Dev DB that ran an older checksum of version 1: reset.

## Tooling

No retired-version lists in code. Boot floor is `min_core_migration_version()`
(newest embedded core file).

`dev-migrate --stamp` records migrations as applied without running them. It
refuses unless `ensure_core_schema_markers`, the owner-RLS epoch and the
platform census pass **and** the live catalog equals what the embedded lane
creates:

| Step | Mechanism |
|---|---|
| Replay | one transaction, always rolled back: live `proxima_*` schemas renamed aside, core + every stamped flavor lane applied into a temp ledger; needs `CREATE` on the database, like a fresh install |
| Compare | routines (`pg_get_functiondef`), aggregates, triggers (+ enabled), rules, policies (table owner as `<table owner>`), relations/columns/collations/RLS flags, constraints, indexes, views, sequences, enums, domains — per replayed schema, both directions |
| Not compared | owners, ACLs, rows a migration seeds |
| Refused, not proven | a database carrying a flavor this binary does not compose: its objects on replayed schemas count as differences |

A marker check alone would have stamped a database that applied the v0.0.15
bytes of 0014: the amended checksum recorded over the old routine bodies. Any
difference refuses; restore from before the amended migration or reset.
