# 07 — Storage

Storage contract for identity, ownership, append-only writes, and typed
sidecars. Exact DDL is `crates/storage-pg/migrations/0001_v008.sql`.

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

`owners.kind` is stored once. Fact tables carry `owner_id NOT NULL` FK.
World is `00000000-0000-0000-0000-000000000001`. No `owner_kind` on memory/goal.

Access uses server-resolved `OwnerRef` → roles. No org column.

`publish_to_world` is an in-place series transfer: `UPDATE owner_id` on
`memory_head`/`goal_head` and every `t` on that handle. Same `(handle, t)`.
Triggers allow that column only; all other memory/goal fields stay append-only.

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
| one core file | v0.0.8 is `0001_v008.sql` |
| composite binary runs both | one `proxima_core` plus N flavor schemas |

<a id="core-tables-abstract"></a>
<a id="core-tables--abstract"></a>

## Core Tables — Abstract

Closed vocabularies are SQL enums.

| Table | Owns |
|---|---|
| `owners` | `owner_id`, `kind`; World seeded |
| `memory_head` | `handle` PK, `kind`, `schema_id`, `owner_id`, head `t` |
| `memory` | `(handle, t)` PK, `UNIQUE(t)`, `schema_id`, `origins[]`, `refs[]`, `blob_id`, `content_id` |
| `content` | owner-scoped payload; `UNIQUE (owner_id, schema_id, content_hash)` |
| `ingest_keys` | `(owner_id, source_id, ingest_key)` → `t` |
| `announce` | `seq`, `op` append\|forget\|erase, `entity` memory\|goal |
| `blob` | cited artefact |
| `closed_handle` | no new pin to any `t` of that handle |
| `goal_head` / `goal` | Goal timeseries; `wake_id`, `write_act_t`, `dependency_t`, `evidence_t` |
| `wake_config` | the one UPDATE table; N Goals share `wake_id`; DELETE RESTRICT |
| `cooled` | forget stub; object key `cold/<owner_hash>/<handle>/<t>` |
| `sketch` | hot one-liner for recall/think (`t` PK = Memory.t or Goal.t); forget deletes |
| embeddings / jobs / heads | independent of graph authorship |
| core sidecars | `agent_note_v1`, `utterance_v1`, `agent_derivation_v1`, `interpretation_v1`, `mcp_call_logged_v1`, `task_goal_v1` |

No `edges`, `memories`, `goals` (plural), `fact_entities`, `fact_receipts`, `cited_objects`, `citation_mappings`, `change_event` tables.

Physical source of truth: `0001_v008.sql`.

<a id="append-only"></a>

## Append-Only

| Operation | Rule |
|---|---|
| `INSERT` | normal write |
| `UPDATE` | `memory` / `goal` / `ingest_keys` / `announce` / `owners` refuse UPDATE. Heads may move `t` only. `wake_config` is the UPDATE table. |
| `DELETE` | forget (hot row, after cold PUT); erase (abandonment only) |

`wipeable := abandoned ∨ (cold ∧ unreferenced ∧ policy)`. World is never abandoned.

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
| writes are replayable | `ingest_keys` |
| forget is cool | PUT `cold/` first, then delete hot; last-t forget deletes `memory_head` |
| hydrate | same `t`; recreates `memory_head` when the series was empty |

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
