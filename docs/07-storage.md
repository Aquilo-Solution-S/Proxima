# 07 — Storage

Storage contract for identity, ownership, append-only writes, and typed
sidecars. The frozen DDL is `crates/storage-pg/migrations/0001_v008.sql`;
the hard-erase witness contract is additive in
`crates/storage-pg/migrations/0005_erased_pin_targets.sql`, and exact
Goal command replay state is additive in
`crates/storage-pg/migrations/0006_v013_goal_replay_declaration.sql`, and
pre-publication upload content identity is additive in
`crates/storage-pg/migrations/0007_upload_content_identity.sql`; the cold
object integrity witness is additive in
`crates/storage-pg/migrations/0008_cold_integrity_digest.sql`.

<a id="id-types"></a>

## ID Types

| ID | Shape | Rule |
|---|---|---|
| `UserId`, `GroupId` | UUIDv7 | external identity |
| `SourceId` | text | stable source-declared id |
| `SchemaId` | text | flavor-qualified; on `memory_head` and each `memory` row (same value) |
| `ToolId` | text | build-time tool id (05 / 12) |
| `MemoryId` | UUIDv7 | `memory.t` |
| `ContentId` | UUIDv7 | `content.content_id` |
| `GoalId` | UUIDv7 | `goal.t` |
| `ErasedPinTarget` | internal `(t, closed kind)` | permanent hard-delete witness; owner-free and not a public edge/pin |
| `ChangeEvent.seq` | UUIDv7 | `announce.seq` |
| `EmbeddingVersion` | integer | independent of entity identity |

`handle` is the series. `t` is the row. `MemoryId` / `GoalId` wrap `t`.

Fact identity is `t`. `ingest_keys` is the only sourced unique.

<a id="identity-rules"></a>

## Identity Rules

| Entity | Identity | Lifecycle |
|---|---|---|
| Fact | fresh `t`; keyless → new `t`; same `(owner, source, ingest_key)` → same `(handle, t)` | immutable; no later `t` on that handle for a Fact rewrite via ingest_key |
| Abstraction / Perspective | `(handle, t)` | later `t` on the same handle is a new version |
| Goal | `(handle, t)` | later `t` on the same handle; terminal admits no later `t` |
| Pin | the `t` stored in `origins` / `refs` | write-time only |
| Blob | `blob_id`; unique `(owner_id, schema_id, content_hash)` | insert-only |
| Embedding | `(entity_id, model_id, embedding_version)` | re-embed writes a new row |

Stateful Fact current-state is a head-by-natural-key query on the sidecar (03).
Ingest of a stateful Fact with empty `handle` reuses the owned NK head; a
miss mints. Flavor code does not JOIN `memory_head`.

<a id="owner-columns"></a>

## Owner Columns

`owners.kind` is stored once — `personal` or `group`, the whole vocabulary.
Fact tables carry `owner_id NOT NULL` FK. No `owner_kind` on memory/goal.

**One owner, one column.** `Surface::owner_column` is `Option<&'static str>`:
a table declares one owner column or none (`None` = reached through the
owner of its key). Several owner relationships on one table become a mapping
table each — own table, own single owner column, keyed to the parent. See
09 §Sidecar Tables.

Access uses server-resolved `OwnerRef` → roles. No org column.

`transfer_to_owner` is an in-place series transfer: `UPDATE owner_id` on
`memory_head` and every `t` on that handle. Same `(handle, t)`. Triggers
allow that column only; all other memory fields stay append-only. Cooled
stubs, `sketch`, embeddings, and exclusively cited blobs and content move
with the series; `ingest_keys` for those `t`s are deleted. The series'
`mcp_call_logged_v1` rows stay put: that audit sidecar is *owner-pinned* —
it carries `actor_upn` and its own `owner_id`, stamped with the owner that
made the call — so a transfer leaves it with the source, and every surface
that reads it keys on that column. Every other sidecar follows the memory. The destination's `owners` row is minted inside the same
transaction (`ensure_owner_row`), which is what keeps the FKs whole. It also
commits paired `transfer` rows into `announce` (prior owner's lane +
destination owner's lane). Goals do not transfer: the verb is memory-only,
and `goal`/`goal_head` refuse UPDATE entirely (`goal_head_t_only` freezes
`goal_head.owner_id` as the DDL backstop).

<a id="storage-layout"></a>

## Storage Layout

| Owner | Namespace |
|---|---|
| core | `proxima_core` (`0001_v008.sql`) |
| `proxima-code` | `proxima_code` |

| Rule | Consequence |
|---|---|
| core owns entity tables | flavors do not redefine `memory` / `goal` |
| flavor owns its sidecars | PK is `t` → `memory(t)` or `goal(t)` |
| one frozen core baseline | `0001_v008.sql`; v0.0.9+ append additive files, never edit it |
| composite binary runs both | one `proxima_core` plus N flavor schemas |

<a id="core-tables-abstract"></a>
<a id="core-tables--abstract"></a>

## Core Tables — Abstract

Closed vocabularies are SQL enums.

| Table | Owns |
|---|---|
| `owners` | `owner_id`, `kind`; no seeded rows — a fresh DB starts empty and rows are minted on first owner write |
| `memory_head` | `handle` PK, `kind`, `schema_id`, `owner_id`, head `t` |
| `memory` | `(handle, t)` PK, `UNIQUE(t)`, `schema_id`, `origins[]`, `refs[]`, `blob_id`, `content_id` |
| `erased_pin_target` | permanent database-only identity witnesses for hard-erased Memory/Goal targets: `t` PK and closed `kind`, with no owner or payload |
| `content` | owner-scoped payload; `UNIQUE (owner_id, schema_id, content_hash)` |
| `ingest_keys` | `(owner_id, source_id, ingest_key)` → `t` |
| `announce` | `seq`, `op` append\|forget\|erase\|transfer, `entity` memory\|goal |
| `blob` | cited artefact |
| `closed_handle` | no new pin to any `t` of that handle |
| `goal_head` / `goal` | Goal timeseries; `wake_id`, `write_act_t`, `dependency_t`, `evidence_t` |
| `goal_replay_declaration` | Goal-keyed immutable command snapshot and prior edge count for exact request-id replay |
| `wake_config` | the one UPDATE table; N Goals share `wake_id`; DELETE RESTRICT |
| `cooled` | forget stub; object key `cold/<t>` — owner-free, so a transfer re-homes the row and never the bytes; `blob_id`, `source_id`, and `ingest_key` copied from the hot row for replay and for source-scope erase |
| `sketch` | hot one-liner for recall/think (`t` PK = Memory.t or Goal.t); forget deletes |
| embeddings / jobs / heads | independent of graph authorship |
| core sidecars | `agent_note_v1`, `utterance_v1`, `agent_derivation_v1`, `interpretation_v1`, `mcp_call_logged_v1`, `task_goal_v1` |

No `edges`, `memories`, `goals` (plural), `fact_entities`, `fact_receipts`, `cited_objects`, `citation_mappings`, `change_event` tables.

Physical schema source of truth: the ordered SQL files in
`crates/storage-pg/migrations/`; `0001_v008.sql` is the frozen baseline.

<a id="append-only"></a>

## Append-Only

| Operation | Rule |
|---|---|
| `INSERT` | normal write |
| `UPDATE` | `memory` / `goal` / `goal_replay_declaration` / `ingest_keys` / `announce` / `owners` refuse UPDATE. Heads may move `t` only. `wake_config` is the UPDATE table. |
| `DELETE` | forget (hot row, after cold PUT); erase (abandonment only). Hard erase records the concrete target's `(t, kind)` witness before deleting it; witness rows are append-only. |

`wipeable := abandoned ∨ (cold ∧ unreferenced ∧ policy)`.

<a id="content-hash-dedup"></a>

## Content-Hash Dedup

Content hashes are blob identity, not Fact identity.

| Surface | Collision |
|---|---|
| `ingest_keys` | same `(handle, t)` |
| `blob` `(owner, schema, hash)` | same `blob_id` |
| UUIDv7 `t` | storage error |

<a id="time-partitioning"></a>

## Time Partitioning

Physical. Hot row is `memory`. Cold object is `cooled` + S3 `cold/`.
Partitioning does not change `t`.

<a id="vector-store-independent"></a>
<a id="vector-store--independent"></a>

## Vector Store — Independent

Independent of entity tables.

| Rule | Consequence |
|---|---|
| no FK from memory to embedding | writes never block on embed |
| `entity_id` is `t` | embeddings can be swept |
| re-embed = new row | memory row unchanged |
| similarity is query-time | never authors a pin |

`embeddings.vec vector(1024)`. Forget drops vectors; hydrate enqueues jobs.

<a id="consequences-of-append-only"></a>

## Consequences Of Append-Only

| Consequence | Effect |
|---|---|
| CDC | `announce.seq` |
| writes are replayable | `ingest_keys`; the handle resolves through `memory` ∪ `cooled`, so a cooled admission still replays |
| forget is cool | lock `(t, owner)`, PUT `cold/`, insert locator, delete hot; on a pre-commit error retain the PUT only when `cooled(t, object_key)` names it; retain on an ambiguous commit outcome. Last-t forget deletes `memory_head`. Refuse if a remaining hot non-Fact would lose `groundingSupport` (no hot pin and no cooled Fact). Forget does not create an erased-target witness. |
| hydrate | same `t`; recreates `memory_head` when the series was empty. An exact sealed cooled snapshot may restore correctly kinded erased targets. Legacy or unwitnessed cold rows are reported as unsupported and remain cooled; they never enter ordinary live-target admission. Hydration preserves a newer existing head. |

Owner-authorized hydration is a bounded Host API repair command. One id and a
set of at most `MAX_MEMORY_HYDRATION_BATCH` ids use the same lifecycle locks
and the existing Postgres hydration atom. Cold format v7 carries the exact
`memory.sidecar_tables` stamp beside its dumps plus every contract-declared
`DumpThenCascade` detail table and its exact zero-or-many rows. Hydration
requires the sidecar dump set to equal the stamp minus declared retained
owner-pinned tables and the detail declaration to equal the frozen schema
contract. Format v6 remains readable only for schemas with no preserved detail
tables; pre-v6 objects are unsupported because they have no authenticated
sidecar stamp. Each newly cooled locator also stores the BLAKE3 digest of its
encoded object. A legacy locator with a NULL digest remains unsupported, and
bytes that do not match a present digest are invalid. A set commits all
owner-visible cooled restorations together; if any
selected object is missing, unsupported, or invalid, the transaction rolls
back and valid cooled items are reported as `NotAttempted`. Hot, absent, and
foreign ids do not mutate state; absent and foreign ids both report `NotFound`.
Missing objects, unsupported cold formats or integrity witnesses, unsupported
sidecar stamps, and invalid objects have distinct typed outcomes. Exact
restore leaves correctly kinded `erased_pin_target` witnesses in place and
reports their count without projecting witness metadata; that count is read
after the complete lifecycle lock union is held and comes from the committed
hydration atom.

Hard erase never rewrites another Memory row's `origins[]` or `refs[]` and
does not cascade or null those source declarations. The witness is technical
metadata only: it is not transferred, forgotten, exported, or projected as a
public Edge/PinNode/MCP/REST field. A public missing target remains the
existing redacted/missing projection.

## Lifecycle Lock Ordering

Per-entity admission, hydration, forget, and single-entity erase share one
per-`t` advisory lock vocabulary. Each of these paths computes its complete
lifecycle target set, sorts and deduplicates it, and acquires that set before
any row or blob lock. A batch hydration computes the union across all selected
admissions and acquires every series handle before any lifecycle target, so
one transaction never extends its advisory set item by item.
Transfer uses the same per-`t` vocabulary with bounded retry: it exclusively
fences both endpoints in sorted owner order, locks the complete sorted series
handle/`t` sets, and rechecks membership before rehoming.
Goal and wake writes use the same union-before-row rule: assignment,
dependencies, evidence, the expected/current Goal head, terminal/write-act
Facts, wake trigger, hard context, and the new Goal `t` are held before
`goal_head`, Goal, or `wake_config` insertion. Memory admissions share a
distinct owner fence, and sourced admissions also share an exact source
fence, before entering the handle/lifecycle order. A whole-owner erase takes
the owner fence exclusively; a source-scope erase takes the owner fence
shared and its source fence exclusively. The fence is taken before the
selection reads anything, so the snapshot is exact by construction: it holds
the Memory/Goal scope as of fence acquisition, and the selected handles and
`t`s are then locked before any deletion or witness work. Memory/Goal and
upload admission take their shared owner fence before first-use owner-row
arbitration. Transfer exclusively fences both endpoints in sorted owner order
before its series locks, then locks its complete sorted series handle/`t` set
and rechecks membership before rehoming. A flavor scope the substrate cannot
see fences itself in its own namespace: Code repository erase takes a fence
(`proxima-code-repo-fence:<owner_kind>:<owner_id>:<repo_id>`) exclusively
before it reads its footprint, and every transaction writing a Memory,
sidecar, admission, or run row carrying that `repo_id` takes the same fence
shared before its handle/`t` locks and revalidates the `repos` row under it.
Shared writers do not wait on each other; the exclusive erase waits for all
of them and is then waited on by every writer after it. Order: owner fence,
source fence, repository fence, handle/`t`, rows. Repository ids are never
hashed into the `t`/handle namespace, distinct repositories take distinct
keys, and a vanished repository row is a typed refusal on every ingest path
rather than an unscoped write.

Upload object keys have their own fence, one rank below the lifecycle set. A
path that is about to decide whether an upload object's bytes may be destroyed
— owner erase, source-scope erase, upload stage, finish and abort, mount
creation, and the in-place upload moves inside transfer — locks the complete
sorted, deduplicated set of `blob_uploads.object_key` values it will decide
about, after its owner/source fence and after its handle and lifecycle `t`
locks, and before any refcount read, any row lock on `blob_uploads`, or any
`cold_purge_pending` insert. The refcount is a query, never a counter: "does
any surviving row under any owner still name this key". Holding the key to
commit is what makes that query a decision rather than an observation — two
owners erasing one mounted object would otherwise each read the other's row
under READ COMMITTED, both withhold, and orphan the bytes. The upload paths
take the key before their own row lock while erase takes it before its
deletes, so the two orders can cross; `40P01` is therefore an expected
outcome of correct code on these paths and every one of them re-runs its
whole transaction under the shared bounded retry budget.

<a id="scaling-envelope"></a>

## Scaling Envelope

Typed sidecars: dozens of tables in a typical flavor mix. Rejected: one JSONB payload column; cross-flavor sidecar writes.

<a id="what-this-doc-is-not"></a>

## What This Doc Is Not

| Not | Source |
|---|---|
| exact DDL | `0001_v008.sql` |
| Rust types | `crates/core/src/` |
| protocol | 14 |
| flavor registry | 03 / 08 |
| compliance | 13 |
