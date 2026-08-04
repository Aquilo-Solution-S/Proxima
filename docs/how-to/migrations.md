# Migration policy

How schema migrations are authored, frozen, and squashed — and why the rules
are what they are. The operator-facing counterpart ("what do I apply to my
database") is `MIGRATING.md`; the release checklist is `RELEASING.md`. This
file is for anyone writing a migration.

## The three rules

1. **A version number is never reused.** Once a file with version `N` has been
   applied to any database you do not personally own — a teammate's dev DB,
   staging, production — its bytes are frozen under that number forever. You
   may delete the file, you may replace it, but the replacement takes a **new,
   never-used version number**. Numbers are cheap; there are 9.2 × 10¹⁸ of
   them. Gaps in the sequence are normal and carry no meaning.

2. **One frozen file per release.** During a release cycle, author as many
   draft files as the work needs. At release preparation, the cycle's drafts
   are squashed into a single `NNNN_vX.Y.Z.sql` under a fresh version number
   (rule 1), and the drafts are deleted. The tagged release ships exactly one
   core migration file. After the tag, that file is frozen — amending it is
   forbidden, full stop.

3. **The migrations directory is history, not documentation.** The
   human-readable answer to "what does the schema look like" is the generated
   schema artifact — `db/schema.core.sql` for core alone, `db/schema.code.sql`
   for what the code flavor adds — regenerated whenever a migration changes
   and verified in CI by replaying `migrations/` from empty. One file per
   source on purpose: a flavor is composed, not welded on, and the artifact
   must show that boundary. The answer to "what did vX.Y.Z change" is that
   release's lane section in `MIGRATING.md`. Nobody should ever need to read
   the migration files in sequence to understand the schema.

## Why these rules — sqlx ledger semantics in five lines

sqlx records every applied migration in `_sqlx_migrations` as
`(version, checksum)`. Two distinct failures can arise, and they are not
symmetric:

- **Orphan row** — the ledger records a version the binary no longer embeds.
  With `ignore_missing = true` (which we set), this is *completely forgiven*.
  Deleting migration files is free.
- **Checksum mismatch** — the binary embeds version `N` with different bytes
  than the ledger recorded. This is `VersionMismatch(N)`, and **nothing
  suppresses it** — not `ignore_missing`, not any flag. Reusing a version
  number is the only way to manufacture this error, and rule 1 is the only
  cure.

This is why the v0.0.7 squash hurt: five drafts (12–15) were folded *into
version 11*, whose checksum was already in every dev and staging ledger. Had
the squash minted version 16 instead, the orphaned rows 12–15 would have been
silently forgiven and no retired-version bookkeeping would ever have existed.

## Lanes and ledgers

Core migrations use small sequential integer versions; flavor migrations use
date-shaped versions. The boundary (`CORE_MIGRATION_VERSION_CEILING`, 9999)
is a real namespace invariant: it is what lets the boot preflight decide
*generically* whether a recorded version is the core lane's business, with no
enumerated lists.

Core records into `public._sqlx_migrations`. Each flavor records into its own
tracking table — `public._sqlx_migrations_proxima_code` for the code flavor —
so each lane is validated against its own author. The flavor tables live in
`public`, not in the flavor's schema, deliberately: destructive flavor
baselines drop the flavor schema, and the ledger must survive the very
migration it records. A database migrated before this split has its flavor
rows moved out of the shared table automatically, once, by the migration
facade.

## The release cycle, step by step

**During the cycle.** Add draft files freely: `0012_add_widget_table.sql`,
`0013_widget_indexes.sql`, … One file per change, so parallel branches never
edit the same file. Draft files may be amended or renumbered as long as no
database beyond your own has applied them; once staging has run a draft,
rule 1 applies to it too — replace it under a new number instead of editing
it.

**At release preparation.** Squash the cycle's drafts into one file under the
next unused version — e.g. drafts 12–14 become `0015_v008.sql` — and delete
the drafts. Verify the squash by replaying `migrations/` from an empty
database and diffing against the schema artifacts. Write the lane section in
`MIGRATING.md`. Tag.

**What each kind of database sees after the squash:**

- *Fresh install* — applies the frozen per-release files in order. Clean.
- *Previous tagged release* — has everything up to the previous lane; applies
  the one new file. This is the supported forward path.
- *Dev/staging that ran the full draft lane* — ledger holds orphans 12–14
  (forgiven) but the schema already matches, so applying 15 would re-run DDL
  and fail. Remedy: **stamp, don't reset** — `sqlx migrate skip` (sqlx ≥ 0.9)
  records 15 as applied without executing it; `dev-migrate --stamp` wraps this
  and purges the orphan rows.
- *Dev/staging that ran a partial draft lane* — stamping would lie (the schema
  is mid-lane). `dev-migrate` refuses to stamp when the schema markers don't
  match and offers the guarded reset instead.

## What the tooling enforces — and what it must never contain

- **No retired-version lists.** History lives in the ledger and in git, not in
  constants. A database in a bad state is detected *generically*: an applied
  core version the embedded migrator doesn't know, or a known version with a
  differing checksum, produces a legible error naming the versions and the
  remedy (stamp or reset). If writing a migration ever seems to require adding
  a version list to the code, the migration workflow is being violated —
  stop and re-read rule 1.
- **The boot floor is derived, not declared.** The `skip_migrations` preflight
  computes its minimum version as the highest version the embedded migrator
  contains. There is nothing to bump at release time and nothing to forget.
- **The schema artifacts are CI-verified.** Replaying `migrations/` from
  empty must reproduce `db/schema.core.sql` and `db/schema.code.sql`; drift
  fails the build. This is also what makes stamping safe — a database is
  stampable exactly when its dumped schema matches.

## Squashing frozen history (rare, deliberate)

Rules 1–2 keep the directory at one file per release, which grows slowly.
If the frozen files themselves ever need collapsing into a new baseline, that
is only safe when **no reachable database sits inside the squashed range** —
for us, pre-1.0, that means every deployment is on the latest tag; later it
means a documented required-stop release, GitLab-style. The baseline takes a
fresh version number (rule 1 has no expiry date), and databases already past
it are stamped, never re-run.
