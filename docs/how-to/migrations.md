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

## v0.0.33

Existing v0.0.32 databases upgrade in place. Two forward core migrations;
released files stay byte-for-byte unchanged. No flavor migration.

| Lane | Migration |
|---|---|
| Core | `0029_v035_cross_store_origins.sql`: `proxima_core.memory_external_origin (t, store, handle)` — the origins a memory takes from other stores ([15 §Other stores](../15-deployment.md#other-stores)); no foreign key, owner RLS of class `memory_owner` |
| Core | `0030_v036_fact_occurred_at.sql`: `memory.occurred_at` — when a Fact occurred ([01 §Properties of an Event](../01-event-source.md#properties-of-an-event)); existing Facts are backfilled with their write time, a `CHECK` keeps it on every Fact and off every Abstraction and Perspective, and the append-only guard freezes it |

## v0.0.32

No migration: v0.0.31 databases run unchanged.

## v0.0.31

Existing v0.0.30 databases upgrade in place. One forward core migration;
released files stay byte-for-byte unchanged. No flavor migration.

The v0.0.31 migration is the named forward cutover for the pre-v0.0.28
outbox format. Before upgrading, drain old undelivered records with
a configured consumer, or retire the owed consumer with
`PublicationConsumerPort::retire`; retirement removes its lanes and settles
records with no remaining undelivered lane. Stop older hosts before running the
migration. It refuses before schema
changes while an old record has an undelivered lane, including the seeded
`default` consumer when no intake was ever configured. Delivered old records remain valid and do not need
to be parsed again. The forward migration then drops the obsolete record-level
claim columns and constraint in place, following the in-place column removal
precedent in `0027_v033_owner_id.sql`; this is an upgrade of existing
databases, not a replacement baseline.

| Lane | Migration |
|---|---|
| Core | `0028_v034_nested_groups.sql`: legacy outbox guard, record-state enum and obsolete claim-column removal; `group_edges` keyed by child group; `group_role_projection` keyed by person/group; trigger-consumed `group_lock_request`; tree locks, kind/forest guards and projection-maintenance triggers |

**Backfill.** Existing person membership rows stay as written. Their relation
roles are joined into one projection row per person/group, including people
not yet registered in `owners`; eager resolution and point probes keep the
v0.0.30 answers.

**Tree.** One parent/relation per group, no self edge/cycle/depth limit. Meet
caps each upward link; join combines every person's relation/path. Parent
members gain no role in a child. A group with a subgroup is not abandoned.

**Transaction.** Raw SQL and host/pool writes acquire root advisory locks
before decision reads; two trees lock in UUID order, then roots are confirmed.
Membership writes require `READ COMMITTED`. Both `REPEATABLE READ` and
`SERIALIZABLE` retain a snapshot predating a waited lock and are refused.

**Authority.** Platform-owned trigger routines have a pinned trusted
`search_path` ending in `pg_temp`; saved/restored platform scope reads the
whole tree under forced RLS and restores the caller's scope. Non-trigger
helpers have no public/runtime execute grant. Owner policies remain group
scoped; projection writes belong to maintenance alone
([10 §Roles inside a host transaction](../10-configuration.md#roles-inside-a-host-transaction)).

## v0.0.30

Existing v0.0.29 databases upgrade in place, in version order. The release
ships one additive core migration and one code-flavor migration. An owner is
one owner id, a bare UUID; the kind (user or group) lives only in
`proxima_core.owners.kind`.

| Lane | Migration |
|---|---|
| Core | `0027_v033_owner_id.sql`: `owners_owner_id_kind_key UNIQUE (owner_id, kind)`; every group a membership names is registered as `group` (the upgrade stops if a membership names an id registered as a user); `group_memberships.group_kind` (always `group`, `group_memberships_group_kind_chk`) with `group_memberships_group_registered_fk (group_id, group_kind) → owners (owner_id, kind)`; `owner_kind` dropped from `delegated_authority_grants`; `source_cursors` keyed `(owner_id, source)`; `publication_origin.original_owner_kind` and its check dropped, `publication_origin_owner_source_idx` rebuilt on `(original_owner_id, source_id, t)` |
| Code flavor | `20261014000020_v033_owner_id.sql`: `owner_kind` dropped from `proxima_code.repos` and `repo_ingestion_runs`; `repos_pkey (owner_id, repo_id)`, `repos_unique_path`, the two run indexes and `runs_repo_fk` rebuilt without it |

**Registry.** A group is registered when it is created (its first admin or
first member); a user when first seen. Only three checks read the kind: the
forwarder policy (a role only in a registered group), the erase precondition
(a user's id needs the host's drop proof, a group's needs zero members, which
ever erase path names it), and the membership foreign key above.

**Fences.** The owner, source and scope advisory-lock keys are built from the
bare id (`proxima-owner-fence:<uuid>`). Stop every older binary before the
upgrade: an older binary's fence no longer excludes a newer one.

**Hashes.** `FactReceiptId`, derived and interpretation ids and the code repo
fingerprint hash the bare id. **A replay across the upgrade no longer
deduplicates:** an ingest replayed after the upgrade with the same idempotency
key as one made before it mints a new id instead of returning the old one.

**Wire.** `X-Proxima-Owner`, MCP space keys, `core_transfer.to_owner`, the
membership tool's `group`, the search output `owner`, CloudEvents
`proximaowner` and change events carry the bare UUID. The old
`personal:<uuid>` / `group:<uuid>` spellings are refused; there is no alias.

Re-running either file changes nothing: every constraint and column is added
or dropped only when needed.

## v0.0.28

Existing v0.0.27 databases upgrade in place, in version order. The release
ships one additive core migration and no flavor migration. Stop every older
binary before it runs: an older drain's claim fails on
`publication_outbox_no_claim_chk` from then on, and a claim one held is handed
back by the backfill.

| Lane | Migration |
|---|---|
| Core | `0026_v032_outbox_consumers.sql`: `proxima_core.publication_consumer (name, registered_at)` seeded with `default`; `proxima_core.publication_delivery`, one lane per (record, consumer) with its own state, claim, lease, attempts and published mark, the checks `publication_delivery_claim_chk` and `publication_delivery_published_chk`, and the partial index `publication_delivery_pending_idx`; `publication_outbox_no_claim_chk CHECK (state <> 'claimed')`; `memory_head.publication_sequence bigint NOT NULL DEFAULT 0` with `memory_head_publication_sequence_chk`, and `memory_head_t_only` replaced so it also refuses a decrease |

**Consumers (`_v032_`).** Delivery state moves from the record onto
per-consumer lanes ([18 §Consumers](../18-fact-outbox.md#consumers)). Every
record that is not `published` gets one `pending` lane of `default`, keeping
its `attempts`, and a `claimed` record is set back to `pending` (its lease
would have expired anyway). The record's own claim columns stay and are unused
from here on. `publication_delivery` is owner-RLS class `fk_parent` of its
record; `publication_consumer` is class `ownerless`, with the metadata read
policy, because a capture reads the consumer set in the writer's owner scope.
Both go through the 0024 per-table building blocks.

**Series position (`_v032_`).** `publication_sequence` counts the listenable
events a series has captured; every existing head starts at `0`, so the first
event captured after the upgrade is `sequence` 1 ([18 §Ordering](../18-fact-outbox.md#ordering)).
Records captured before carry no `subject`/`sequence`; v0.0.31 refuses an
upgrade while any such record is still owed to a consumer. After the upgrade,
`ReceivedEvent::from_record` requires the pair and refuses a malformed or
missing one.

Re-running the file changes nothing: tables, columns and indexes are created
only when missing, the seed and the backfill skip what exists, the check is
dropped and added again, and the RLS appliers replace their own policies.

## v0.0.27

Existing v0.0.26 databases upgrade in place, in version order. The release
ships one additive core migration and no flavor migration.

The file is labelled `_v031_`: the label counts merged migrations, and `_v030_`
is `0024` ([v0.0.26](#v0026)).

| Lane | Migration |
|---|---|
| Core | `0025_v031_goal_text_outbox_receipt.sql`: nullable `proxima_core.goal.text` with `goal_text_nonblank_chk CHECK (text IS NULL OR length(btrim(text, E' \t\n\r\x0b\f')) > 0)`, backfilled from `proxima_core.goal_replay_declaration`; the unique constraint `goal_head_handle_owner_key` on `goal_head (handle, owner_id)` and the foreign key `goal_owner_matches_head_fk` from `goal (handle, owner_id)` to it (`NOT VALID`); and `publication_outbox_published_chk` relaxed to `CHECK ((state = 'published') = (published_at IS NOT NULL))`; the sidecar table `proxima_core.goal_change_v1` of the `core/goal-change-v1` Fact, with the enums `goal_change_kind` and `goal_pending_work` |

**Goal text (`_v031_`).** A Goal version stores its prompt text. `NULL` means
unknown, never empty: a version that no declaration can speak for (written
before v0.0.13 recorded declarations, or a row written outside the Goal write
verbs) stays `NULL`, and the database refuses a blank text ([06 §Goal
text](../06-goals-and-self.md#goal-text) defines blank). Create, modify and
decompose store the text they were given. Transition and achieve store exactly
their prior version's text, `NULL` included: the Goal write verbs carry it, and
the database does not check the carry. The foreign key
`goal_owner_matches_head_fk` keeps every new version in a series of its own
owner ([06 §Goal text](../06-goals-and-self.md#goal-text)). The table stays
append-only, so a stored text never changes.

**Backfill.** For every `goal` row, `text` is the text of the latest
declaration among the rows of the same `handle` and the same `owner_id` with
`t` at or before the row's own that carries a string text that is not blank
(the same expression as the check). The text sits at a fixed place of the
declaration envelope (`goal_write/replay.rs`, `{version, verb, ...}`):

| `declaration ->> 'verb'` | JSON path |
|---|---|
| `create` | `#>> '{draft,text}'` |
| `modify` | `#>> '{replacement,text}'` |
| `decompose_child` | `#>> '{payload,text}'` |

`transition` and `achieve` declare no text, so their versions inherit the
text of the version before. A modify never reaches back to an earlier version,
a blank or non-string text declares nothing, and a row with no declaration
before it in its series stays `NULL`: the text is never guessed from the title.
The Goal writers keep a series to its head's owner, but before 0025 the
database did not, and a raw insert could add a row of another owner to a
series. Such a row takes only its own owner's texts, so the upgrade never
shows one owner's text in another owner's reads or export.
Re-running the file changes nothing: `ADD COLUMN IF NOT EXISTS` skips the
column and its check together, and only rows whose text is still `NULL` are
written. An empty database has no rows to visit.

**Outbox check (`_v031_`).** The Fact outbox is delivered in process to a host
intake, so a published record has no broker stream position to name ([18
§Outbox Row and State Machine](../18-fact-outbox.md#outbox-row-and-state-machine)).
`publication_outbox_published_chk` required `published_at`, `published_stream`
and `published_seq` for a published row; it now requires `published_at` alone,
in both directions. The old check let a record that is not published carry
`published_at` while a receipt field was `NULL`. No Proxima write does that, but
a hand-written row can, so the migration first clears `published_at` on every
record that is not published; the captured event is untouched. The two receipt
columns stay, and nothing writes them any more. The check is dropped and added
again, so a re-run changes nothing.

**Goal change Fact (`_v031_`).** A goal write that inserts a Goal row also
writes a `core/goal-change-v1` Fact when the runtime has an outbox intake
([18 §Goal changes on the feed](../18-fact-outbox.md#goal-changes-on-the-feed)).
Its sidecar `proxima_core.goal_change_v1` is `(t, series, goal_t, prior_t,
revision, state, change_kind, parent_t, pending)`, append-only, with `t` a
foreign key to `proxima_core.memory` and `revision integer NOT NULL CHECK
(revision >= 1)`, the version's position in its series. Two checks tie the
optional columns to the kind:
`goal_change_v1_pending_chk CHECK ((change_kind = 'pause') = (pending IS NOT
NULL))` and `goal_change_v1_parent_chk CHECK ((change_kind = 'decompose_child')
= (parent_t IS NOT NULL))`, so `pending` exists on a pause alone and is
required there, and `parent_t` likewise on a decomposed child. There is no key
into `goal`: a goal erase never has to reach the Fact. The table is wired as
`utterance_v1` and `write_act_v1` are: a `proxima_core.flavor_surface` row for
the core flavor, the declaration and presence triggers of
`crates/storage-pg/src/integrity.rs` (pasted verbatim and pinned by
`generated_declaration_triggers_are_the_migration_text` and
`generated_presence_triggers_are_the_migration_text`), and owner-RLS of class
`fk_parent` through the 0024 per-table building blocks. The section is idempotent:
the enums are created only when missing, the table with `IF NOT EXISTS`, the
triggers with `OR REPLACE` or dropped first, and the surface row skips a
conflict; a second run changes nothing. A database that holds no goal-change
Fact before the upgrade holds no row after it, and no existing row is
rewritten.

**Locks.** The migration takes `ACCESS EXCLUSIVE` on `proxima_core.goal` for
its transaction: a validating scan for the check, then one `UPDATE` that writes
a new row version for every Goal version it can give a text (the old versions
are left for autovacuum). The append-only trigger `goal_append_only` is
disabled for that one statement and enabled again before the transaction
commits, so no concurrent write sees the gap. The unique constraint builds its
index on `proxima_core.goal_head` (one scan of the heads), and the foreign key
is added `NOT VALID`, so it scans no existing row; a row an earlier raw insert
put into another owner's series stays, and every new insert is checked against
`goal_head`. The outbox check takes `ACCESS EXCLUSIVE` on
`proxima_core.publication_outbox`, and its rows are scanned twice: once by the
`UPDATE` (which rewrites no row of a database Proxima alone wrote) and once to
validate. The table holds the pending backlog and whatever published history
retention leaves. The goal-change section creates one empty table and a
trigger on `proxima_core.memory`, which takes `SHARE ROW EXCLUSIVE` on it
briefly; it reads and rewrites no row. No other table is touched.

**Operator action.** None when boot applies migrations. A deployment with
`PROXIMA_SKIP_MIGRATIONS=true` must run the migration step before the v0.0.27
binary starts: boot checks for the column, its constraint and the foreign key
`goal_owner_matches_head_fk` and refuses a database without them ("apply
migrations before boot"). `ensure_declaration_triggers` also refuses a database
without the declaration triggers of `proxima_core.goal_change_v1`.

**Stop v0.0.26 processes before the upgrade.** A v0.0.26 binary cannot start
against the upgraded database, with or without `PROXIMA_SKIP_MIGRATIONS`: both
boot paths run `ensure_core_ledger_compatible` first, the ledger records core
version 25 and a v0.0.26 binary does not embed it, so boot fails with `core
migration preflight failed: internal storage error: database core-migration
ledger does not reconcile with this binary: core versions [25] are recorded as
applied but this binary does not embed them (...)`. A v0.0.26 process that is
already running keeps serving, but its Goal inserts name no `text` column, so
every version it writes stores a `NULL` text: after it pauses, resumes,
abandons or achieves a Goal, the Goal's text is unknown from that version on,
until a modify by a current binary declares one again. Stop every v0.0.26
process sharing the database before the migration step, and do not leave one
running through the rollout.

## v0.0.26

Existing v0.0.25 databases upgrade in place, in version order. The release
ships one additive core migration and no flavor migration.

The file is labelled `_v030_`, not `_v026_`. A label counts merged migrations,
not tags ([v0.0.24](#v0024) rolled up `_v024_` through `_v029_`), and `_v026_`
is `0020`, which shipped in v0.0.24. The label stays as merged; an applied
ledger records it.

| Lane | Migration |
|---|---|
| Core | `0024_v030_owner_rls_table_installer.sql`: enum `proxima_core.owner_rls_class` (`owner_id`, `fk_parent`, `ownerless`, `memory_owner`), `proxima_core.install_owner_rls_table(schema, table, class[, owner_column])` and `proxima_core.assert_owner_rls_census(schema)`; `install_owner_rls` is re-created on top of the class code both installers share, with its signature, checks and policy expressions unchanged |

**Per-table owner RLS (`_v030_`).** No table or row changes. A host that adds
one table per migration calls `install_owner_rls_table` for that table instead
of restating its whole schema; the other tables' policies are not touched
([09 §Owner RLS](../09-developing-flavors.md#owner-rls)). A flavor that
installed its schema with `install_owner_rls` keeps its policies; calling
either installer again re-creates them from the current class code.

**Locks.** Additive: one enum type, six new functions (not executable by
`PUBLIC`), and `install_owner_rls` re-created with the same signature. No table,
row, policy or index is read or written, so no table lock and no backfill.
Existing policies stay as they are until an installer is called again.

**Operator action.** None when boot applies migrations. A deployment with
`PROXIMA_SKIP_MIGRATIONS=true` must run the migration step before the v0.0.26
binary starts: boot checks for the type and functions and refuses a database
without them ("apply migrations before boot").

## v0.0.25

No core or flavor migration ships in this release. Existing v0.0.24 databases
remain compatible and require no reset.

## v0.0.24

The first tag after v0.0.23. It rolls up the files labelled `_v024_` …
`_v029_`: their pull requests merged without bumping `RELEASE_VERSION`, so no
tag was cut between them, and this release ships several files per lane. The
labels stay as merged; an applied ledger records them. Existing databases
upgrade in place, in version order.

| Lane | Migration |
|---|---|
| Core | `0019_v025_definer_search_path.sql`: pins `pg_temp` last in the eleven platform-owned integrity routines from `0014`; preserves bodies, owners and privileges |
| Core | `0020_v026_embedding_chunks.sql`: adds zero-based `embeddings.chunk_ordinal` to the primary key; existing rows become chunk 0 without re-embedding |
| Core | `0021_v027_query_stopwords.sql`: replaces `lexical_query_text` with dominant configured-language stopword analysis; preserves the function's identity, owner and privileges |
| Core | `0022_v028_coarse_pin_lock.sql`: `memory_pin_checks` stops taking per-target advisory locks after 256 in a transaction holding SHARE ROW EXCLUSIVE or stronger on `memory`, `cooled` and `goal`; preserves the function's identity, owner, ACL and `search_path` |
| Core | `0023_v029_kind_scoped_owner_rls.sql`: every `proxima_core` owner-keyed table follows the one authorization rule (a scope acts on kind K when its limit for that direction is ≥ K). `cooled`, `memory_head`, `sketch` by their `kind`; embedding tables and `projection` by their memory's kind; `goal_head`, `wake_config` as Goal; `announce`, `content`, `closed_handle` by the entity they name. A table in no class refuses the migration. `install_owner_rls`: `memory_owner_tables` follow the parent memory's kind and may carry `owner_id` |
| Code flavor | `20260927000020_v024_file_class.sql`: enum `proxima_code.file_class` (`source`, `generated`, `vendored`, `lockfile`) and a nullable `code_chunk_v1.file_class`; `embed_text` is re-added so a lockfile chunk embeds `(lockfile) path:start-end` only |
| Code flavor | `20260928000020_v025_ingest_run_class_counts.sql`: eight integer columns on `repo_ingestion_runs` (`files_source`, `files_generated`, `files_vendored`, `files_lockfile`, `chunks_source`, `chunks_generated`, `chunks_vendored`, `chunks_lockfile`), each default 0 |
| Code flavor | `20260929000020_v029_kind_scoped_owner_rls.sql`: re-runs the installer; `projection` and the two Self sidecars move to `memory_owner_tables` |

**Code chunk class (`_v024_`).** Re-adding the STORED `embed_text` column
rewrites `code_chunk_v1` once, under an ACCESS EXCLUSIVE lock — budget a full
table rewrite. Stored `embed_text` values stay byte for byte on every existing
chunk, so no vector goes stale. Chunks written before this file carry no class
and read as `source`. A file gets its class the next time its content is
ingested; to class a whole repository at once, erase it and ingest again
([connect-agent](../getting-started/connect-agent.md)). The column is nullable
so a cold dump taken before it still hydrates.

**Ingest-run class counts (`_v025_`).** `proxima-code_get_ingest_run` reads
them as `files_by_class` and `chunks_by_class`. A run that has not succeeded,
and every run written before this file, reads as zeros. No rewrite of
`code_chunk_v1`.

**Embedding chunks (`_v026_`).** `embedding_heads` still selects a version.
New writes store every chunk and advance the head in one transaction.
Per-dimension HNSW indexes remain over chunk rows. Cold archives remain v8:
forget stores distinct spaces, drops all vectors, and hydration queues one job
per space (see 07 §Vector Store).

**Query stopwords (`_v027_`).** Existing text vectors and per-row language
stamps remain unchanged. Query analysis is `STABLE` and runs as the caller;
ranked queries prepare it once per statement (see 10 §Memory hybrid search).

**Coarse pin lock (`_v028_`).** No table or row changes. Usage: [15 §Bulk
Memory writes](../15-deployment.md#bulk-memory-writes).

**Kind-scoped owner RLS (`_v029_`).** No table or row changes; only policy
expressions. New derived Content is keyed by its admission's kind (an
Abstraction and a Perspective with one payload get two rows), so a writer
never reuses Content it cannot read; existing Content rows keep their hash,
and a new derivation equal to one made before this release gets its own row.
Before this file a Fact-only scope read an owner's Abstraction, Perspective
and Goal rows in `memory_head`, `sketch`, `cooled`, `announce`, `content` and
the embedding tables. A flavor that lists `memory_owner_tables` keeps its old
policies until it calls the installer again
([09 §Owner RLS](../09-developing-flavors.md#owner-rls)).

## v0.0.23

No core or flavor migration ships in this release. Existing v0.0.22 databases
remain compatible and require no reset.

## v0.0.22

No core or flavor migration ships in this release. Existing v0.0.21 databases
remain compatible and require no reset.

## v0.0.21

No core or flavor migration ships in this release. Existing v0.0.20 databases
remain compatible and require no reset.

## v0.0.20

| Lane | Migration |
|---|---|
| Core | `0018_v020_owner_rls_installer.sql`: `proxima_core.install_owner_rls(schema, owner_id_tables, fk_parent_tables, ownerless_tables[, memory_owner_tables])`, the owner-RLS installer flavor migrations call ([09 §Owner RLS](../09-developing-flavors.md#owner-rls)); no table or row changes |
| Code flavor | `20260924000020_v020_owner_rls_installer.sql`: the v0.0.15 owner-RLS block as one installer call; rekeys `execution_plan_v1` (an Abstraction sidecar) on its own `t` instead of a Fact reference |

Existing databases upgrade in place. A flavor adopting the installer does so
in a **new** migration; its released v0.0.15 file stays byte for byte.

A flavor built with `NamedMigrator::flavor(id, migrator)` records on
`public._sqlx_migrations_<id>` (§Lanes). A flavor that recorded on core's
`public._sqlx_migrations` gets its rows copied there on its next migration
run (boot, unless `skip_migrations`; or `dev-migrate`), before its migrator
first reads the new ledger; the rows stay on core's ledger too, so an older
binary re-runs nothing. Every run also revokes non-owner writes on every
ledger, core's included.

Replicas booting together serialize every migration transaction on
`pg_advisory_xact_lock('proxmigr')` (bounded by the 5 s `lock_timeout`);
before v0.0.20 a replica could re-apply migrations the first had not yet
committed and fail its boot.

## v0.0.19

No core or flavor migration ships in this release. Existing v0.0.18 databases
remain compatible and require no reset.

## v0.0.18

No core or flavor migration ships in this release. Existing v0.0.17 databases
remain compatible and require no reset.

## v0.0.17

No core or flavor migration ships in this release. Existing v0.0.16 databases
remain compatible and require no reset.

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
  every recorded core version to match an embedded migration checksum; a
  flavor ledger must be one its lane continues (§Ledger lineage).
- Checksum mismatch: fatal. Do not edit an applied file.

## Lanes

Core versions are small integers (`0001_v008.sql`). Flavor versions are
date-shaped. Boundary: `CORE_MIGRATION_VERSION_CEILING` (9999).

Core ledger: `public._sqlx_migrations`.
Flavor `<id>`: `public._sqlx_migrations_<id>`, `-` spelled `_`
(`NamedMigrator::flavor`, `flavor_ledger_table`); the code flavor's is
`public._sqlx_migrations_proxima_code`. A flavor on core's ledger boots with a
warning.

## Ledger lineage

Every migrator keeps `ignore_missing`: a ledger row the binary does not ship is
normal, because two releases' fleets share one database until the older is
retired, and a lane may shed a file. Before a flavor lane runs, the migration
run reads its ledger under the migration lock and refuses a lane that does not
continue it, before applying anything (`MigrationError::Ledger`):

| `LedgerConflict` | Shape | Remedy |
|---|---|---|
| `Replaced` | the lane's first migration was never applied, and the ledger records versions this binary does not ship | a replaced baseline, in either direction: retire the release that does not match, or reset the lane's schemas and ledger before the new baseline first boots (rule 3) |
| `Diverged` | a pending migration is older than a recorded one this binary does not ship | a later release squashed or renumbered this one's files: retire this release |
| `Unapplied` | `skip_migrations` only: a lane migration its ledger does not record | run the migration step first |

A fresh ledger, an upgrade, an older release beside a newer one, and a lane
that shed a file with only newer migrations pending all boot. Core's shared
ledger is not checked: its rows cannot be attributed to a lane, and core has
its own preflight. A host or flavor lane recording there is unchecked too;
`NamedMigrator::flavor` moves it onto its own ledger. Not caught: a lane
squashed in part into a newer version, whose kept first migration looks like
an upgrade. Only a publish-time check that each release's lane extends the
previous one's sees that.

## Migration plan

`proxima::host::pending_migrations(pool, sources)` returns what a migration run
on the same database would apply, and applies nothing. `sources` is what
`flavors` is to `run_core_and_flavor_migrations`; core is always first.

| | |
|---|---|
| Result | `Vec<PendingMigration { ledger, version, description, checksum }>`: core, then each source in the given order, inside a ledger in the order the run walks the migrator (ascending by version for `sqlx::migrate!`; a migrator built by hand keeps the order it was given, for the plan as for the run); down migrations excluded. `ledger` is the schema-qualified table (`public._sqlx_migrations` for core), spelled as `flavor_ledger_table` spells it |
| Rules | the runner's, from its helpers. Duplicate versions and shared flavor ledgers refuse before any query, a table is matched by the name Postgres resolves, not by its spelling (`_sqlx_migrations_host`, `public."_sqlx_migrations_host"` and `PUBLIC._SQLX_Migrations_Host` are one ledger; `"Host"` and `"host"` are two), and a spelling outside Postgres' identifier rules is compared as written. A flavor on its own ledger counts that ledger (absent = empty) plus the rows of core's ledger that the run's ledger preparation would copy for its versions: it copies and creates nothing, so a flavor whose rows sit only on core's ledger has nothing pending. A flavor on core's ledger is read there |
| Errors | what the run returns for the same database: `Ledger` (`Replaced`, `Diverged`); `Core` or `Flavor` carrying `Dirty` (a `success = false` row) or `VersionMismatch` (a changed checksum); `CorePreflight` (an unknown or amended core row). No new variant. The answer is the whole list or an error |
| Snapshot | takes the migration lock under the run's 5 s `lock_timeout`, then reads every ledger in one `REPEATABLE READ, READ ONLY` transaction, core's checks included. A plan waits for a run mid-source and reports the state after its commit. A snapshot, not a reservation: a run checks everything again under the lock. The connection is closed, not pooled, when the caller drops the plan while it waits |
| Rights | `SELECT` on the ledgers, which ledger preparation leaves to the runtime role: the previous release's runtime role can ask. No DDL, no write. The ownership of the migrating role, which a run checks, is not |

The plan reads ledgers, not the catalog: a ledger row without its objects is
invisible to it (`ensure_core_schema_current` probes those).

**Not a compatibility claim.** The plan says what would apply, not whether the
previous release's binary tolerates it, and nothing in a migration file can:
v0.0.15's schema changes are additive, yet older binaries cannot resume after
activation (§v0.0.15); v0.0.16 changed the cold-archive record format, which no
migration records (§v0.0.16). A coordinated cutover overrides any reading of
the files, so Proxima ships no per-migration "safe for the previous binary"
marker.

## Reset

`dev-migrate --reset` (local hosts, `PROXIMA_RESET_CONFIRM`) drops every
`proxima_*` schema with `CASCADE` and deletes Proxima's ledger rows. A consumer
that embeds Proxima shares `public._sqlx_migrations` with its own lanes; the
`CASCADE` would also drop what those lanes built on Proxima while their rows
still claim it, so their migrator never re-creates it.

| Invocation | Objects outside `proxima_*` depend on it (`pg_depend`) |
|---|---|
| `--reset` | refuses before dropping anything; lists each object and what it depends on |
| `--reset --reset-dependent-lanes` | drops them too, then deletes every ledger row no Proxima lane owns: `public._sqlx_migrations` and every other `public._sqlx_migrations_*` table; refuses up front if the role cannot delete from one |

Counted (direct `pg_depend` edges; their own dependents go too): an outside
object depending on a Proxima object (FK, view, trigger, policy, column of a
Proxima type), and an object on a Proxima table depending on a non-extension
user object outside (a consumer trigger on `proxima_core.memory`). Not
detectable: a consumer object on a Proxima table that references only Proxima
or built-in objects. After the opt-in, the consumer's lanes re-run from
scratch: reset their remaining objects first — the reset cannot tell which lane
built which object.

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
