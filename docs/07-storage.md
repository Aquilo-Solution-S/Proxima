# 07 — Storage

Abstract storage layer. No domain specifics. Defines IDs, core
tables, append-only discipline, and the **independence of the
vector store** from the entity tables.

## ID types

```rust
type OrgId    = Uuid;        // UUIDv7, external (usermanager)
type UserId   = Uuid;        // UUIDv7, external (usermanager)
type GroupId  = Uuid;        // UUIDv7, external (usermanager)

type SourceId = String;      // stable, source-declared ("system", "forgejo-aquilo")
type SchemaId = String;      // binary-scoped, per 03 ("code/forgejo-commit")
type ToolId   = String;      // per 05 ("ask-question")

struct ContentHash([u8; 32]);   // BLAKE3

type EventId       = ContentHash;   // hash(source_id, owner, payload) — dedup at re-receipt
type SourceBatchId = Uuid;          // UUIDv7, declared by source at emit (Q6)

type MemoryId          = Uuid;   // UUIDv7 for Fact, Abstraction, Perspective alike
type GoalId            = Uuid;   // UUIDv7
type CitedObjectId     = Uuid;   // UUIDv7 (11)
type CitationMappingId = Uuid;   // UUIDv7 (11)

type EdgeId = EdgeIdValue;
enum EdgeIdValue {
    EventSourceAuthored(ContentHash),   // hash(source_id, target_id, relation) — dedup at re-receipt
    OperatorAuthored(Uuid),              // UUIDv7 for OperatorFtoA / OperatorAtoP / PerspectiveLink / Supersedes
}

type SchemaVersion    = u32;
type EmbeddingVersion = u32;
```

UUIDv7 everywhere mutable identity is generated — sortable by time
prefix, 16 bytes, no separate ULID dependency. ContentHashes only
where deterministic re-receipt must dedup at insert time (Events;
EventSource-authored edges). `MemoryId` is a 16-byte UUIDv7 for every
memory regardless of kind; Fact identity is **not** the content hash
— Facts carry an `event_id` FK to `events` for the re-receipt dedup.

## Identity rules

| Entity | Id derivation | Immutable after |
|---|---|---|
| Event | `hash(source_id, owner, payload)` | insert |
| Fact | fresh UUIDv7; `event_id` FK to `events` is the re-receipt dedup key | insert |
| Abstraction | fresh UUIDv7 | insert; supersede via new id |
| Perspective | fresh UUIDv7 | insert; supersede via new id |
| Goal | fresh UUIDv7 | insert; supersede via new id |
| Source batch | UUIDv7 declared by source at emit (Q6, [01](docs/01-event-source.md)). Engine validates uniqueness within `(source_id, owner)` and tracks lifecycle in `source_batches` ([04](docs/04-consolidation.md)) | insert |
| Cited object | fresh UUIDv7 (11). Idempotent within Owner on `(schema_id, content_hash)` from `CitedObjectPayload::idempotency_key` | insert |
| Citation mapping | fresh UUIDv7 (11). UNIQUE per `memory_id` — at most one mapping per Fact | insert |
| Edge | EventSource-authored: `hash(source_id, target_id, relation)` (dedup). Operator-authored (`OperatorFtoA` / `OperatorAtoP` / `OperatorAtoGoal` / `PerspectiveLink` / engine `Supersedes`): fresh UUIDv7. | insert |
| Change event | fresh UUIDv7 (`seq`); server-generated at write, monotonic by time. Same DB transaction as the entity / edge insert it announces (14 §Consistency). | insert |
| Schema version | `(SCHEMA_ID, SCHEMA_VERSION)` const on the payload impl — `FactPayload`, `AbstractionPayload`, `PerspectivePayload`, `GoalPayload`, `CitedObjectPayload`, `CitationMappingPayload` (all required, 03, 06, 08, 11). Version lives in sidecar table name, not on parent row. | compile-time; sidecar SQL migration tracked via `schema_migrations` |
| Tool | `tool_id` (declared) | insert (registry) |
| Embedding | `(entity_kind, entity_id, embedding_version, model_id)` | insert; re-embed = new row |

Immutability lives at the **identity layer**. Schema migration
re-shapes typed sidecar payload bytes; Fact identity (`id`,
`event_id`, `citation_mapping_id`, `observed_at`, `schema_id`) does
not move.

## Owner columns

Every row that carries an `Owner` (per 01) stores it as three
columns:

```
owner_principal_kind  -- {User, Group}
owner_principal_id    -- UserId | GroupId
owner_org_id          -- OrgId
```

Plus a check constraint enforcing valid `(kind, id)` pairing.

## Storage layout

Sidecars are namespaced by **per-flavor pg schema** (Postgres
namespace, not data-shape "schema" — they collide on the word but
not the concept). Every flavor's sidecars live under
`proxima_<FLAVOR_ID>.<table>`:

```
proxima_code.fact_forgejo_commit_v3
proxima_code.abstraction_bug_fix_cluster_v1
proxima_learning.fact_lecture_note_v1
proxima_general_reasoning.perspective_self_model_v1
```

The substrate's own tables (events, memories, edges, goals,
change_event, schema_migrations, source_batches, source_batch_f2a,
read_scope_matrix, embeddings, …) live in `proxima_core`.
Cross-flavor joins are unrestricted — pg-schema separation is for
catalog organisation and per-flavor permission boundaries, not for
query isolation.

The `proxima_flavor!` macro generates each flavor's migration
prefix from `FLAVOR_ID`; a composite binary that links N flavors
runs `N+1` pg schemas (one per flavor plus `proxima_core`). This
is the canonical multi-tenant Postgres pattern ("schemas as
namespaces"). Boring on purpose; what it buys:

- `pg_dump --schema=proxima_code` cleanly carves a flavor.
- `SET search_path TO proxima_code, proxima_core` makes a flavor's
  surface inspectable in isolation.
- Per-flavor `GRANT` / `REVOKE` makes the trust-tier story
  ([13 §Trust](docs/13-flavor-marketplace.md#trust)) enforceable
  in the database, not just in Rust.
- Catalog noise stays bounded per-flavor as the binary's flavor
  mix grows.

## Core tables — abstract

```
events(
    event_id              pk,
    source_id,
    source_batch_id,
    owner_*,
    schema_id,
    schema_version,
    observed_at,  occurred_at,
    payload_ref           -- into schema sidecar (per 03)
)

memories(
    memory_id             pk,             -- UUIDv7
    owner_*,
    schema_id             NOT NULL,        -- every memory carries a registered schema ([03](docs/03-schema-registry.md))
    schema_version        NOT NULL,        -- writer-known payload version returned by Query / Subscribe
    created_at,
    -- kind-discriminated columns (exactly one variant present per row):
    -- Fact variant:
    event_id              nullable FK events,  -- Fact: NOT NULL, UNIQUE among Facts; A/P: NULL
    citation_mapping_id   nullable FK citation_mappings,  -- Fact: NOT NULL (11); A/P: NULL
    -- Derived variant (Abstraction | Perspective):
    kind                  nullable {Abstraction, Perspective},  -- Derived: NOT NULL; Fact: NULL
    text                  nullable,         -- Derived: NOT NULL; Fact: NULL
    operator_kind          nullable {FtoA, AtoP},  -- Derived: NOT NULL; Fact: NULL
    model_id              nullable,         -- Derived: NOT NULL; Fact: NULL
    prompt_version        nullable,         -- Derived: NOT NULL; Fact: NULL
    personality_id         nullable,         -- Derived: NOT NULL — which personality flavor produced this (08); Fact: NULL
    personality_state_hash nullable,         -- Derived: NOT NULL; Fact: NULL
    supersedes            nullable          -- Derived only: UUIDv7 of prior memory in same personality_id lineage; Fact: NULL
)
-- Entity definition in [02-memory.md](docs/02-memory.md#the-core-entity).
-- Check constraint: exactly one variant present per row.
--   Fact:      event_id NOT NULL AND citation_mapping_id NOT NULL
--              AND kind IS NULL AND text IS NULL AND operator_kind IS NULL
--              AND model_id IS NULL AND prompt_version IS NULL
--              AND personality_id IS NULL AND personality_state_hash IS NULL
--              AND supersedes IS NULL
--   Derived:   kind NOT NULL AND text NOT NULL AND operator_kind NOT NULL
--              AND model_id NOT NULL AND prompt_version NOT NULL
--              AND personality_id NOT NULL AND personality_state_hash NOT NULL
--              AND event_id IS NULL AND citation_mapping_id IS NULL
-- Fact immutability: the Fact branch's `supersedes IS NULL` is the
-- storage-level enforcement of the trauma-test invariant
-- (02 §Re-derivation and supersession). Stateful Fact projections
-- ("current revision of file X") express heads via head-by-natural-key
-- queries on the schema's sidecar (03 §Stateful Fact schemas), not
-- via lineage replacement.
-- Supersession constraint (Derived only): when supersedes IS NOT NULL,
-- the prior row's personality_id must match this row's personality_id.
-- Cross-personality supersession is rejected at this layer; user-API
-- writes that intend cross-personality replacement bypass via the
-- explicit `Core(User(u))`-authored path (see 02 §Re-derivation and
-- supersession).

edges(
    edge_id               pk,
    source_kind           NOT NULL,             -- EntityKind (02): {Fact, Abstraction, Perspective, Goal}
    source_memory_id      nullable FK memories, -- set when source_kind ∈ {Fact, Abstraction, Perspective}
    source_goal_id        nullable FK goals,    -- set when source_kind = Goal
    target_kind           NOT NULL,             -- EntityKind
    target_memory_id      nullable FK memories,
    target_goal_id        nullable FK goals,
    relation,
    owner_*,
    authored_by,                                -- carries the reasoning concept; replaces the old citation_id
    created_at
)
-- No `citation_id` column. authored_by is the reasoning concept (11).
-- CHECK: exactly one of (source_memory_id, source_goal_id) is non-null
--        and matches source_kind. Same for target. Validates the
--        EntityId sum type per 02 §Edges.

goals(
    goal_id                pk,             -- UUIDv7
    schema_id              NOT NULL,        -- every goal carries a registered GoalPayload (06)
    owner_*,
    text                   NOT NULL,
    state                  {Active, Paused, Achieved, Abandoned},
    supersedes             nullable,
    authorship_kind        {User, System, External},
    -- System authorship discriminated columns:
    authorship_origin      nullable {Operator, Tool},  -- NOT NULL when authorship_kind = System
    authorship_operator_id nullable,         -- NOT NULL when authorship_origin = Operator
    authorship_tool_id     nullable,         -- NOT NULL when authorship_origin = Tool
    -- Operator authorship columns (only valid when authorship_origin = Operator):
    operator_kind          nullable {AtoGoal},  -- NOT NULL when authorship_origin = Operator
    model_id               nullable,         -- NOT NULL when authorship_origin = Operator
    prompt_version         nullable,         -- NOT NULL when authorship_origin = Operator
    personality_id         nullable,         -- NOT NULL when authorship_origin = Operator — producing personality flavor (08)
    personality_state_hash nullable,         -- NOT NULL when authorship_origin = Operator
    created_at
)
-- Entity definition in [06-goals-and-self.md](docs/06-goals-and-self.md#goal-entity).
-- Per-schema sidecar: goal_<schema>_v<n>(goal_id pk FK, …)
-- Version is implicit in sidecar table membership.
-- Check constraint:
--   authorship_kind=User      => authorship_origin IS NULL AND authorship_operator_id IS NULL
--                                AND authorship_tool_id IS NULL AND operator_kind IS NULL
--                                AND model_id IS NULL AND prompt_version IS NULL
--                                AND personality_id IS NULL AND personality_state_hash IS NULL
--   authorship_kind=System    => authorship_origin NOT NULL
--                                AND (authorship_origin = Operator => operator_kind NOT NULL
--                                     AND model_id NOT NULL AND prompt_version NOT NULL
--                                     AND personality_id NOT NULL AND personality_state_hash NOT NULL
--                                     AND authorship_operator_id NOT NULL)
--                                AND (authorship_origin = Tool => authorship_tool_id NOT NULL)
--   authorship_kind=External  => authorship_origin IS NULL AND authorship_operator_id IS NULL
--                                AND authorship_tool_id IS NULL AND operator_kind IS NULL
--                                AND model_id IS NULL AND prompt_version IS NULL
--                                AND personality_id IS NULL AND personality_state_hash IS NULL
-- Supersession constraint: when supersedes IS NOT NULL and both rows'
-- authorship is Operator-origin, their personality_id must match.
-- Cross-personality goal supersession is rejected at this layer; only
-- user-authored writes (authorship_kind=User) may supersede an
-- Operator-origin goal under a different personality (06 §Goal-write API).

goal_parents(
    goal_id, parent_goal_id,
    pk(goal_id, parent_goal_id)
)

schema_migrations(
    name                  pk,            -- migration filename / id
    applied_at,
    checksum
)
-- Schemas live in code (FactPayload / AbstractionPayload /
-- PerspectivePayload impls per 03/08); only their sidecar SQL
-- migrations are tracked here.

tools(
    tool_id               pk,
    name,
    schema_id,
    registered_at
)
-- No `registrant` column. Build-time only — the linked flavor crate
-- is the registrant; nothing is registrable from outside the binary
-- ([08](docs/08-core-and-flavors.md)).

cited_objects(
    cited_object_id       pk,             -- UUIDv7
    schema_id             NOT NULL,        -- registered CitedObjectPayload (11)
    owner_*,
    content_hash          NOT NULL,        -- BLAKE3 from idempotency_key
    created_at,
    UNIQUE (owner_principal_kind, owner_principal_id, owner_org_id,
            schema_id, content_hash)
)
-- Per-schema sidecar: cited_<schema>_v<n>(cited_object_id pk FK, …)
-- Version is implicit in sidecar table membership.

citation_mappings(
    citation_mapping_id   pk,             -- UUIDv7
    schema_id             NOT NULL,        -- registered CitationMappingPayload (11)
    memory_id             NOT NULL FK memories,
    cited_object_id       NOT NULL FK cited_objects,
    owner_*,
    created_at,
    UNIQUE (memory_id)                      -- one mapping per Fact
)
-- Per-schema sidecar: citation_<schema>_v<n>(citation_mapping_id pk FK, …)
-- Version is implicit in sidecar table membership.

source_batches(
    id                    pk,             -- UUIDv7, == source_batch_id on events / facts
    source_id             NOT NULL,
    owner_*,
    opened_at             NOT NULL,
    closed_at             nullable          -- NOT NULL once source signals batch-complete (04)
)
-- Fixed shape, not flavor-typed. Domain-specific batch metadata
-- belongs on a CitedObject the batch's Facts cite (11).

source_batch_f2a(
    batch_id                FK source_batches.id,
    operator_id             OperatorId,
    prompt_version          PromptVersion,
    model_id                ModelId,
    personality_id          PersonalityId,
    personality_state_hash  Hash32,
    head_memory_id          nullable FK memories.memory_id,
    run_at                  NOT NULL,
    PRIMARY KEY (
        batch_id,
        operator_id,
        prompt_version,
        model_id,
        personality_id,
        personality_state_hash
    )
)
-- Per full invocation key. Empty means that exact prompt/model/
-- personality snapshot has not run for that batch/operator; a row
-- means it has.

read_scope_matrix(
    owner_*,
    self_personality      PersonalityId NOT NULL,
    other_personality     PersonalityId NOT NULL,
    allowed               bool NOT NULL,
    updated_at            NOT NULL,
    PRIMARY KEY (owner_principal_kind, owner_principal_id, owner_org_id,
                 self_personality, other_personality)
)
-- Per-Owner adjacency matrix governing cross-personality retrieval
-- of A/P/Goals (see 02 §Read-scope matrix). Identity diagonal
-- (self_personality == other_personality) is always allowed = true
-- and enforced as a CHECK; F is below the matrix and always shared.
-- Hashed into personality_state_hash at snapshot time so a matrix
-- toggle producing new admissible sources is a different invocation
-- key (04 §Personality state).

change_event(
    seq                   pk,                  -- UUIDv7, server-generated at write
    owner_*,
    kind                  {EntityAppend, EdgeAppend},

    -- kind = EntityAppend:
    entity_kind           nullable,             -- EntityKind (02): {Fact, Abstraction, Perspective, Goal}
    entity_memory_id      nullable FK memories, -- set when entity_kind ∈ {Fact, A, P}
    entity_goal_id        nullable FK goals,    -- set when entity_kind = Goal
    entity_schema_id      nullable,
    entity_schema_version nullable,             -- known by the writer at insert time; sidecar version on the wire
    entity_personality_id nullable,             -- set when entity_kind ∈ {Abstraction, Perspective} or when Goal authorship is Operator-origin
    supersedes_memory_id  nullable,             -- prior memory_id, when this row supersedes a head (same personality_id)
    supersedes_goal_id    nullable,             -- prior goal_id, when this row supersedes a head (same personality_id)

    -- kind = EdgeAppend:
    edge_id               nullable FK edges,
    edge_relation         nullable,
    edge_source_kind      nullable,             -- EntityKind
    edge_source_memory_id nullable,
    edge_source_goal_id   nullable,
    edge_target_kind      nullable,
    edge_target_memory_id nullable,
    edge_target_goal_id   nullable
)
-- The protocol-level outbox (14 §Consistency). One row per
-- EntityAppend / EdgeAppend on the wire (14 §Subscribe). Written in
-- the same DB transaction as the announced entity / edge row.
-- CHECK:
--   kind=EntityAppend => entity_kind NOT NULL AND entity_schema_id NOT NULL
--                        AND exactly one of (entity_memory_id, entity_goal_id)
--                        non-null and matches entity_kind
--                        AND supersedes_* (if present) matches entity_kind
--                        AND all edge_* / edge_relation NULL.
--   kind=EdgeAppend   => edge_id NOT NULL AND edge_relation NOT NULL
--                        AND edge_source_kind / edge_target_kind NOT NULL with
--                        exactly one of their (memory_id, goal_id) non-null and
--                        matching the kind
--                        AND all entity_* / supersedes_* NULL.
-- Edge fields are denormalized off `edges` so the publisher can fan
-- out and filter ([14 §Subscribe filters](docs/14-protocol-surface.md#subscribe)) without joining hot tables;
-- replay of engine history is self-contained on this table alone.
```

`payload_ref` on `events` resolves to the sidecar table for
`(schema_id, schema_version)` per [03](docs/03-schema-registry.md).
Fact, Abstraction, Perspective, Goal, CitedObject, and
CitationMapping sidecars are all insert-only; lifetime is tied to
the parent row.

## Append-only

| Operation | Allowed |
|---|---|
| `INSERT` | yes |
| `UPDATE` | no |
| `DELETE` | no, except the explicit GDPR erasure path |

State transitions are **new rows with `supersedes`**, not row
updates. Active set queries filter `WHERE NOT EXISTS (… supersedes
== this.id)` (see [06](docs/06-goals-and-self.md) for the Goal form; [02](docs/02-memory.md) for memories).

Genuinely INSERT only on memories and goals; the only legitimate
DELETE is GDPR erasure. Schema migration is pure insert-into-new-sidecar
+ delete-from-old-sidecar, with no UPDATE on parent rows.

## Content-hash dedup

`event_id` and `edge_id` collisions on insert are silent drops. Re-
receipt of an observation produces identical hashes by construction;
no error surface, no conflict resolution.

## Time partitioning

`events`, `memories`, `edges`, `change_event` partition by
`observed_at` / `created_at` / `seq` month. Cold partitions are
read-only; supersession keeps current state addressable without
rewriting history.

## Vector store — independent

The vector store is **not part of the entity tables.** Separate
schema, separate lifecycle, separate backend if desired.

```
embeddings(
    entity_kind          {Fact, Abstraction, Perspective, Goal},   -- EntityKind per 02
    entity_id,
    embedding_version,
    model_id,
    vector,
    embedded_at,
    pk(entity_kind, entity_id, embedding_version, model_id)
)
```

Properties:

- **No FK from entity → embedding.** The reverse direction
  (embedding → entity) is the only one. Entity writes never block
  on embedding.
- **Re-embedding = new row.** `embedding_version` bump; entity row
  unchanged.
- **Multi-model.** Multiple `model_id` rows per entity coexist
  (e.g. small + matryoshka).
- **Backfillable.** New entity types or new models trigger
  background sweeps; correctness of the entity graph is independent.
- **Pluggable backend.** pgvector today, dedicated store later. Not
  load-bearing for entity correctness.

What is **not** in the vector store: edges. Per [02](docs/02-memory.md) trauma findings,
memory edges must be operator- or LLM-justified, never auto-wired
by similarity. Cosine proximity is a query-time tool; never a
persisted relation.

## Consequences of append-only

Direct results of the discipline above:

- MVCC bloat bounded; vacuum cost bounded; no churn-driven index
  fragmentation.
- Bulk ingest via `COPY`; sidecar tables are pure inserts.
- Read-replica trivial — historical queries have no staleness
  concern.
- Cache invalidation reduces to "row not yet inserted".
- `change_event` **is** the CDC stream. The protocol publisher
  ([14 §Consistency](docs/14-protocol-surface.md#consistency-strong-write-to-stream-via-outbox)), search indexers, embedding workers, and
  replicas tail it directly — no separate CDC apparatus.

## Scaling envelope

Worst-case multiplication (10 flavors × 10 schemas × 3 live
versions × 6 payload kinds = 1.8k tables) overstates the realistic
catalog by orders of magnitude. Pure-personality flavors author
**zero** sidecars. Pure-cognition flavors author **zero**
sidecars. A reality-bringing flavor like Code authors ~5
Fact/Abstraction schemas. Most schemas live at v1 their whole
life. Realistic envelope: **~75 tables at the v1 mix, low
hundreds at high-mix deployments.**

What we **don't** do, and why:

- **No partition-by-version collapse.** `PARTITION BY (schema_version)`
  on a parent `(kind, schema_id)` table is tempting — it would fold
  N version tables into one logical table with N partitions. But
  partitions must share column shape, which forces the parent
  to `body jsonb`. That reinvents the rejected JSONB-blob composition
  approach (project memory: strict typing wins). Typed sidecars
  per version stay; the table count is a deliberate trade.
- **No cross-flavor sidecar sharing.** A flavor cannot author rows
  into another flavor's sidecar — see
  [08 §Composite discipline](docs/08-core-and-flavors.md#composite-discipline).
  Sharing erases migration ownership (who runs the v2 cutover? who
  backfills?). Cross-flavor *reads* are free; cross-flavor *writes*
  are forbidden.

What we do at high mix:

- **Per-flavor pg schema namespacing** (§Storage layout) keeps the
  catalog organised and inspectable per-flavor.
- **Version pruning.** Schema migration is INSERT-into-new-sidecar
  + DROP-old-sidecar (§Append-only); a v_(n-2) sidecar is dropped
  once its last row migrates to v_n. Three live versions is a
  ceiling, not a floor.
- **Per-flavor archival.** `pg_dump --schema=proxima_<flavor>` lifts
  archival to flavor granularity rather than per-binary.

If a deployment reaches ~10k sidecar tables (≥ 100 flavors with
heavy schema churn — outside any v1 envelope), the lever is more
aggressive version pruning, not architecture change. Postgres
handles 10k tables; pg_dump and catalog queries get sluggish well
before "broken" but stay functional. The envelope is documented,
not defended.

## What this doc is not

- Not a physical schema. Column types, indexes, partition strategy
  details land at implementation time.
- Not a query API. Operator and retrieval interfaces live in their
  owning components (02 / 04 / 06).
- Not a storage backend choice. The contract above is satisfied by
  any append-only relational store with side-table extensibility
  for typed payloads.

## Anchors

- `id-types`
- `identity-rules`
- `owner-columns`
- `storage-layout`
- `core-tables-abstract`
- `append-only`
- `content-hash-dedup`
- `time-partitioning`
- `vector-store-independent`
- `consequences-of-append-only`
- `scaling-envelope`
- `what-this-doc-is-not`
