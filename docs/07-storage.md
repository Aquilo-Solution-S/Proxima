# 07 — Storage

Storage contract for core identity, ownership, append-only writes, and
typed sidecar placement. Exact columns, indexes, triggers, and enum
definitions live in `crates/storage-pg/migrations/` and Rust storage
types.

<a id="id-types"></a>

## ID Types

| ID | Shape | Rule |
|---|---|---|
| `UserId`, `GroupId` | UUIDv7 | external identity from usermanager |
| `SourceId` | text | stable source-declared id |
| `SchemaId` | text | flavor-qualified, binary-scoped id (03) |
| `ToolId` | text | build-time declared tool id (05 / 12) |
| `ReceiptId` | content hash | hash of source, Owner, and payload for re-receipt dedup |
| `MemoryId` | UUIDv7 | Fact, Abstraction, and Perspective identity |
| `GoalId` | UUIDv7 | Goal identity |
| `SourceBatchId` | UUIDv7 | source-declared batch id; unique within `(source_id, Owner)` |
| `CitedObjectId` | UUIDv7 | cited object identity (11) |
| `CitationMappingId` | UUIDv7 | Fact citation mapping identity (11) |
| `ChangeEvent.seq` | UUIDv7 | server-generated pull-log order key |
| `EmbeddingVersion` | integer | embedding lifecycle, independent of entity identity |

UUIDv7 is the default for generated mutable identities: sortable by time,
16 bytes, no separate ULID dependency. Content hashes are reserved for
deterministic re-receipt dedup only.

Fact identity is the UUIDv7 `MemoryId`, not the content hash. Receipt
metadata lives in `fact_receipts`; `receipt_id` proves admission only.

<a id="identity-rules"></a>

## Identity Rules

| Entity | Identity rule | Lifecycle rule |
|---|---|---|
| Fact receipt | deterministic `ReceiptId` | duplicate insert is replay |
| Fact | fresh `MemoryId`; optional `receipt_id` | immutable; no supersession |
| Abstraction | fresh `MemoryId` | supersession writes a new row |
| Perspective | fresh `MemoryId` | supersession writes a new row |
| Goal | fresh `GoalId` | supersession writes a new row |
| Edge | the row is its own identity: `(source, target, kind)` | insert-only; a replayed write re-asserts the same primary key |
| Source batch | source-declared `SourceBatchId` | core lifecycle row, not flavor-typed |
| Cited object | fresh `CitedObjectId`; idempotent by payload key | insert-only |
| Citation mapping | fresh `CitationMappingId`; one per Fact | insert-only |
| Embedding | `(entity_kind, entity_id, embedding_version, model_id)` | re-embed writes a new row |

Immutability lives at the identity layer. Schema migration moves typed
sidecar bytes; entity identity, Owner, citations, and provenance do not
move.

Stateful Fact schemas express current state by head-by-natural-key
queries on sidecars (03 §Stateful Fact schemas), never by replacing or
superseding Facts.

<a id="owner-columns"></a>

## Owner Columns

Rows that carry an `Owner` store two identity columns:

| Column | Meaning |
|---|---|
| `owner_kind` | `world`, `personal`, or `group` |
| `owner_id` | `NULL` for `world`; UserId or GroupId otherwise |

`OwnerRef` is the storage owner handle (doc 01 §Owner; the Owner=OwnerRef
collapse removed the tenant field from Core — no org column exists). Access
predicates and identity comparisons (operator gates, edge scoping, dedup
keys) use `owner_kind` + nullable `owner_id` with null-safe equality.
Edges are source-owned; target rendering is separately redacted.

<a id="storage-layout"></a>

## Storage Layout

Core rows and core sidecars live in `proxima_core`. Flavor sidecars live
under one Postgres schema per linked flavor:

| Owner | Sidecar namespace |
|---|---|
| core memory | `proxima_core.agent_note_v1`, `proxima_core.agent_derivation_v1`, `proxima_core.interpretation_v1`, `proxima_core.utterance_v1` |
| core goals | `proxima_core.goal_*_v1` |
| `proxima-code` | `proxima_code.*` |

Postgres schemas are catalog namespaces, not payload schemas. The payload
schema registry is build-time Rust metadata (03 / 08).

Rules:

| Rule | Consequence |
|---|---|
| core owns entity tables | flavors do not redefine Memory / Goal / the edge index |
| core owns generic agent memory sidecars | one `proxima_core` migration stream, starting at `0001_init.sql` |
| flavor owns its sidecars | migration ownership stays local |
| cross-flavor reads are allowed | query composition can span all linked flavors |
| cross-flavor sidecar writes are forbidden | one flavor never writes another flavor's typed rows |
| composite binary runs linked migrations | one `proxima_core` schema plus N flavor schemas |

<a id="core-tables-abstract"></a>
<a id="core-tables--abstract"></a>

## Core Tables — Abstract

Closed DB vocabularies are PostgreSQL enums. Do not model closed
storage values as `text` plus membership `CHECK`. `CHECK` remains for
shape, subset, range, and cross-column rules. Open identifiers remain
text: schema ids, tool ids, model ids, vendors, handles,
paths, and payload text.

| Table family | Owns | Contract |
|---|---|---|
| `fact_receipts` | source receipt and observed payload metadata | optional admission receipt for accepted Fact payloads |
| `memories` | Fact / Abstraction / Perspective identity | common identity, Owner, schema, and lifecycle metadata |
| `edges` | the connection index over Memories, Goals and Fact-entity heads | no id, no payload, no sidecar; PK is `(source_kind, source_id, target_kind, target_id, kind)`; `kind` is the closed enum `origin`/`reference`; owner is always the source owner; existence, endpoint-kind agreement, layering, and the self-loop refusal are enforced by CHECKs and a trigger; rebuildable from node content |
| `goals` | Goal identity and lifecycle | distinct entity; typed GoalPayload sidecar; supersession-only lifecycle |
| `cited_objects` | bibliographic cited-object identity | Owner-scoped idempotency by payload key |
| `citation_mappings` | citation mapping for a Fact or an Abstraction | at most one mapping per memory |
| `source_batches` | core source-batch lifecycle | fixed shape; domain metadata belongs on cited objects |
| `change_event` | change-event pull log | same transaction as the announced entity write; an edge append is announced with the whole edge (source kind/id, target kind/id, edge kind), not a handle to it |
| `schema_migrations` | applied SQL migrations | tracks physical sidecar/core migration files |

Memory-specific rules:

Physical SQL encoding: `memories.kind` stores `Fact` / `Abstraction` / `Perspective` as `proxima_core.entity_kind`. `receipt_id` is optional metadata and is never the Fact discriminator.

| Kind | Parent row | Sidecar | Supersession |
|---|---|---|---|
| Fact | `memories` Fact branch | required `FactPayload` sidecar | forbidden |
| Abstraction | `memories` derived branch | required `AbstractionPayload` sidecar | schema/owner lineage |
| Perspective | `memories` derived branch | required `PerspectivePayload` sidecar | schema/owner lineage |

Goal-specific rules live in [06](06-goals-and-self.md#goal-entity).
Citation rules live in [11](11-citations.md). The edge model lives in
[16](16-edges.md); its layering rule is restated in
[02](02-memory.md#the-directionality-rule).

Physical shape source of truth:

| Surface | Source |
|---|---|
| columns and constraints | `crates/storage-pg/migrations/` |
| storage write semantics | `crates/storage-pg/src/` |
| typed ids and payload traits | `crates/core/src/` |
| protocol contract | `14-protocol-surface.md` |

<a id="append-only"></a>

## Append-Only

| Operation | Rule |
|---|---|
| `INSERT` | normal write path |
| `UPDATE` | whitelisted columns only (owner transfer, tombstone, citation attach, compliance supersedes clear); DB triggers reject all other column mutations on F/A/P rows and typed sidecars |
| `DELETE` | compliance erasure only |

Facts are immutable observations. A/P and Goals revise by new row plus
supersession. Stateful Fact projections calculate heads from sidecar
natural keys. Schema migration inserts into new sidecars and retires old
sidecars; parent identity rows stay fixed.

The only legitimate cognitive-history delete is explicit compliance
erasure: whole abandoned group Owner, verified dropped personal Owner,
or source-object scope inside that abandoned/dropped Owner (13 §Operations).
Live owners refuse.

<a id="content-hash-dedup"></a>

## Content-Hash Dedup

Content hashes are dedup keys, not entity identity, except where the ID
is explicitly defined as a content hash.

| Surface | Collision behavior |
|---|---|
| Fact receipt re-receipt | silent replay/drop |
| edge re-assertion (structural PK) | silent replay/drop |
| UUIDv7 entity id collision | storage error |

No conflict-resolution protocol is attached to deterministic re-receipt.
The same source payload means the same observation.

<a id="time-partitioning"></a>

## Time Partitioning

Time partitioning is a physical implementation concern. The abstract
contract allows hot/cold partitioning for append-heavy tables such as
`fact_receipts`, `memories`, `edges`, `goals`, and `change_event`.

Rules:

| Rule | Consequence |
|---|---|
| partitioning is not identity | moving partitions never changes ids |
| cold partitions are read-only | historical queries stay stable |
| supersession remains logical | current state is a query over append-only rows |

<a id="vector-store-independent"></a>
<a id="vector-store--independent"></a>

## Vector Store — Independent

The vector store is independent from entity tables.

| Rule | Consequence |
|---|---|
| no FK from entity to embedding | entity writes never block on embedding |
| embedding references entity id | embeddings can be swept, rebuilt, or dropped |
| re-embedding writes a new row | entity row does not change |
| latest head is metadata | `embedding_heads` can be rebuilt from `embeddings` |
| multiple models may coexist | model comparison and rollout do not rewrite entities |
| backend is pluggable | pgvector today, dedicated store later |

Embeddings may point at Facts, Abstractions, Perspectives, and Goals.
Edges are not embedded: they carry no content to embed. Similarity is
query-time evidence and never authors a connection — a similarity score that
is worth persisting is an Abstraction citing its computation record
(16 §Computed Scores Are Abstractions), not an edge.

Postgres implementation:

| Surface | Contract |
|---|---|
| extension | `CREATE EXTENSION IF NOT EXISTS vector`; local/CI DBs inherit pgvector 0.8.0 from `template1` |
| column | `proxima_core.embeddings.vec vector(1024)` |
| dimension | fixed 1024 (`mistral-embed`); no `dim` column |
| key | `(entity_kind, entity_id, embedding_version, model_id)` |
| head | `embedding_heads(entity_kind, entity_id, model_id) -> embedding_version` |
| Fact write | memory row + receipt/sidecar are authoritative; embedding may be absent |
| derived write | memory row + typed sidecar are authoritative; embedding may be rebuilt |
| write | validate entity owner/text/live, take `(entity_kind, entity_id, model_id)` advisory lock, append next version, advance head |
| index | `idx_embeddings_vec_hnsw` using `hnsw (vec vector_cosine_ops)` |
| ranking | authorized eligible entities join current heads before bounded vector candidate `LIMIT`; score `1 - (vec <=> query)` |

<a id="consequences-of-append-only"></a>

## Consequences Of Append-Only

| Consequence | Effect |
|---|---|
| CDC is explicit | `change_event` is the change-event pull log (14 §Consistency) |
| writes are replayable | idempotency and content hashes handle re-receipt |
| caches are simple | invalidation is "row not yet inserted" |
| replicas are simple | historical rows do not mutate |
| migrations preserve identity | sidecar bytes move; entity ids stay fixed |

Append-only does not mean no cleanup. Compliance erasure and sidecar
version pruning are explicit lifecycle operations, not ordinary entity
mutation.

<a id="scaling-envelope"></a>

## Scaling Envelope

Typed sidecars deliberately create more tables than a JSONB blob design.
That table count buys queryable typed payloads, migration ownership, and
flavor-local schema evolution.

Bounds:

| Case | Expected shape |
|---|---|
| v1 flavor mix | dozens of sidecar tables |
| high-mix deployment | low hundreds |
| pathological churn | prune old sidecar versions before changing architecture |

Rejected collapses:

| Option | Rejected because |
|---|---|
| one JSONB payload column | loses typed sidecar contract |
| partition-by-version parent table | forces shared column shape or JSONB body |
| cross-flavor sidecar sharing | erases migration ownership |

Per-flavor Postgres schemas keep catalog inspection and archival bounded.

<a id="what-this-doc-is-not"></a>

## What This Doc Is Not

| Not | Source of truth |
|---|---|
| exact physical schema | `crates/storage-pg/migrations/` |
| exact Rust type definitions | `crates/core/src/` |
| query API | 14 and storage/query modules |
| flavor schema registry | 03 and 08 |
| compliance API | 13 |

This doc is authoritative for storage principles and invariants only.

## Anchors

- `id-types`
- `identity-rules`
- `owner-columns`
- `storage-layout`
- `core-tables-abstract`
- `core-tables--abstract`
- `append-only`
- `content-hash-dedup`
- `time-partitioning`
- `vector-store--independent`
- `vector-store-independent`
- `consequences-of-append-only`
- `scaling-envelope`
- `what-this-doc-is-not`
