# 09. Developing Flavors

> New to flavor authoring? Start with [tutorials/build-first-flavor.md](tutorials/build-first-flavor.md), then return here for the complete checklist.

## Contract

Flavor = build-time vocabulary crate.

| Owns | Examples |
|---|---|
| Payload schemas | `FactPayload`, `AbstractionPayload`, `PerspectivePayload`, `GoalPayload` |
| Sidecar storage | SQL tables + typed PG insert/load impls |
| Connections | reference fields declared by `references()` on those payloads |
| Tools | MCP tools under the flavor prefix |
| Sources/operators | Domain ingestion and F/A/P/G derivation code |
| Migrations | One per flavor migrator, global SQLx version namespace |

Core owns substrate invariants, entity rows, owner scope, registry freeze,
storage ports, protocol verbs, wake runtime, Goal lifecycle, and core
tools (see 08).

## Layout

In-repo flavor:

```
flavors/<name>/
  Cargo.toml
  migrations/
    <global-version>_<description>.sql
  src/
    lib.rs
    migrations.rs
    payloads/
      mod.rs
    ingest/
      mod.rs
      pg_sidecars.rs
    mcp/
      mod.rs
  tests/
```

Out-of-tree flavor (host-colocated; no in-repo crate):

```
src/flavor.rs
migrations/
```

Compiling witnesses: `flavors/code` (flavor crate) and `apps/proxima-mcp` (host).

## Build Order

1. Pick one stable `flavor_id`.
2. Define typed payload structs.
3. Implement payload traits and schema-owned keys.
4. Write sidecar SQL tables.
5. Implement PG sidecar insert/load traits.
6. Register schemas/tools with `proxima_flavor!`.
7. Wrap the flavor in `FlavorBundle`.
8. Add ingestion/operators/tools.
9. Add tests.
10. Run workspace verification.

## Namespace

| Item | Rule |
|---|---|
| Flavor id | `kebab-case`; stable persisted prefix |
| Schema ids | `<flavor_id>/<local-schema-vN>` |
| MCP tools | provider-safe `<flavor_id>_<tool>` |
| SQL schema | `<flavor_id>` converted to snake case |
| Rust crate | `proxima-<local>` for in-repo first-party flavors |

`proxima_schema_id!("x")` derives `CARGO_PKG_NAME + "/x"`. Use literal
ids when crate name and flavor id intentionally differ.

## Payload Traits

Facts:

```rust
impl FactPayload for DocumentFiledV1 {
    const SCHEMA_ID: &'static str = "my-flavor/document-filed-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn receipt_key(&self) -> Vec<u8> {
        let mut key = PayloadKeyBuilder::new(Self::SCHEMA_ID, Self::SCHEMA_VERSION);
        key.field_str("source_path", &self.source_path);
        key.finish()
    }

    fn render(&self) -> String {
        format!("Document filed: {} ({})", self.title, self.source_path)
    }

    fn sidecar_table() -> Option<&'static str> {
        Some("my_flavor.document_filed_v1")
    }
}
```

Rules:

| Payload kind | Required key/text |
|---|---|
| Fact | `receipt_key()` + `render()` |
| Abstraction | immutable `text` on memory row + typed sidecar |
| Perspective | immutable `text` on memory row + typed sidecar |
| Goal | `goal_key()`; title/text live on `goals` |

No `serde_json::Value` payload fields. No generic canonical payload
encoder. Keys are schema-owned semantic identity bytes, built with
`PayloadKeyBuilder` — including the optional ones, which have their own
methods rather than a per-flavor spelling (see *Key Selection* below).

## Declaring references

A flavor does not register a connection vocabulary and cannot invent an edge
kind. It declares which of its payload's own fields point at other nodes, and
ingest turns each declaration into exactly one `reference` index row, inside
the node write's own transaction:

```rust
impl PerspectivePayload for CodeWorkAssignmentV1 {
    // ...
    fn references(&self) -> Vec<PayloadReference> {
        vec![
            PayloadReference::memory(
                "target_perspective_memory_id",
                EntityKind::Perspective,
                MemoryId::new(self.target_perspective_memory_id),
            ),
            PayloadReference::memory(
                "work_item_memory_id",
                EntityKind::Fact,
                MemoryId::new(self.work_item_memory_id),
            ),
        ]
    }
}
```

`references()` is available on all four payload traits and defaults to none.

| Constructor | Address | Binding |
|---|---|---|
| `PayloadReference::memory` | a `memory` row (`t`) | pins that observation |
| `PayloadReference::goal` | a `goal` row (`t`) | pins that Goal |

The only binding is `ReferenceBinding::Pin`.

Rules worth internalizing before designing a flavor's graph:

- **The kind follows the operation.** `origin` comes from a write's
  `derived_from`; `reference` comes from `references()`. Nothing else writes
  an edge, and nothing takes a kind as an argument.
- **Multiplicity lives in the payload.** Ten call sites from chunk A to chunk
  B are **one** index row and ten entries in A's payload. The index answers
  "is there a connection"; the node answers "what it is".
- **The node-home test.** If a connection needs to carry something —
  a reason, a confidence, a byte range, a score — that something belongs in a
  node. If no existing node owns the statement, the flavor is missing a node,
  not an edge kind. (`proxima-code/work-assignment-v1` exists for exactly this
  reason: neither the plan Abstraction nor the worker Perspective owned the
  targeting claim.)
- **Rebuildability.** `memory.origins` / `memory.refs` are a function of
  node content. Re-deriving pins from payloads must reproduce the same set.

## Key Selection

Key = replay/idempotency contract.

| Fact type | Key fields |
|---|---|
| External observation | source-natural key + source version |
| Stateful Fact | natural key + content/revision marker + state |
| Action/request Fact | request idempotency key |
| Result Fact | request memory id + status + artifact/log identity |
| Goal payload | payload-specific lifecycle identity, not title/text |

Embedded hosts create product-authored Goals through
`Engine::create_goal(GoalCreateRequest::product(...))`. The helper calls
`GoalPayload::goal_key()`, validates the registered Goal schema, applies
the stable request id, and records the Self assignment on the Goal row
(`assignment_perspective_id`), from which the index entry follows; host apps
do not insert `proxima_core.goal` rows directly.

Include `SCHEMA_ID` and `SCHEMA_VERSION` through `PayloadKeyBuilder::new`.
Never derive keys from arbitrary JSON serialization.

Optional key fields have blessed spellings — `field_option_str`,
`field_option_uuid`, `field_option_bool`, `field_option_time`,
`field_option_u8`/`u32`/`i32`/`i64`/`u64`/`usize`, `field_option_bytes`.
Use them, and never invent a per-flavor absence encoding: not the empty
string, not a zero sentinel, not skipping the field when it is `None`.
Absent, `None`, and `Some(default)` are three different statements about
the world and the builder gives them three different keys, so a field
nobody asked about does not replay onto a field observed to be empty.
Switching an existing schema from a hand-rolled encoding to
`field_option_*` changes its keys, which re-mints every Fact that used
it — so that switch rides a `SCHEMA_VERSION` bump.

## Sidecar Tables

One sidecar-backed payload schema maps to one sidecar table.

```sql
CREATE SCHEMA IF NOT EXISTS my_flavor;

CREATE TABLE my_flavor.document_filed_v1 (
  t uuid PRIMARY KEY
    REFERENCES proxima_core.memory(t),
  source_path text NOT NULL,
  title text NOT NULL
);
```

Rules:

| Rule | Source |
|---|---|
| A/P sidecars required | 03 §Sidecar tables |
| Fact sidecars required when schema has typed payload | 03 §Sidecar tables |
| Closed vocabularies use SQL enums | 07 §Core tables |
| No `extra json/jsonb` | 03 §Sidecar tables |
| No sidecar-only identity | 07 §Identity rules |
| Validate value-bearing integer widths | avoid silent `*_saturating` clamps |

## PG Sidecars

Implement insert and readback for every payload registered with a PG
sidecar registry.

Preferred direct row mapping:

```rust
proxima::flavor::pg_sidecar! {
    payload: DocumentFiledV1,
    row: DocumentFiledRow,
    kinds: [Fact],
    table: "my_flavor.document_filed_v1",
    key: memory_id,
    fields: {
        source_path => source_path: (text),
        title => title: (text),
    },
}
```

Reference: `flavors/code/src/ingest/pg_sidecars.rs`.

Macro contract:

| Key | Meaning |
|---|---|
| `payload` | payload type implementing `FactPayload` / `AbstractionPayload` / `PerspectivePayload` |
| `row` | private `sqlx::FromRow` struct generated by the macro |
| `kinds` | one or more memory payload kinds valid for this row |
| `table` | schema-qualified sidecar table |
| `key` | key column, normally `memory_id` |
| `fields` | payload field → SQL column + column kind |

Column kinds:

| Kind | Rust field | SQL shape |
|---|---|---|
| `uuid`, `opt_uuid`, `uuid_array` | `Uuid`, `Option<Uuid>`, `Vec<Uuid>` | `uuid`, `uuid[]` |
| `text`, `opt_text`, `text_array` | `String`, `Option<String>`, `Vec<String>` | `text`, `text[]` |
| `bool`, `f32`, `timestamptz` | scalar | `boolean`, `real`, `timestamptz` |
| `decimal`, `opt_decimal` | `rust_decimal::Decimal` | `numeric` |
| `naive_date`, `opt_naive_date` | `time::Date` | `date` |
| `bytea32` | 32-byte data | `bytea` with 32-byte validation |
| `u32_as_i32`, `u32_as_i64`, `u64_as_i64` | unsigned Rust integers | checked SQL integer width |
| `opt_u32_as_i32`, `opt_u32_as_i64`, `opt_u64_as_i64` | the same as `Option<u32>` / `Option<u64>` | same widths, nullable |
| `enum { ... }`, `enum_copy { ... }` | Rust enum | PostgreSQL enum cast through text |
| `jsonb`, `opt_jsonb` | `serde_json::Value` | protocol/metadata only; not a payload escape hatch |

Do not use `u32_as_i32_saturating` or `u64_as_i64_saturating` for
value-bearing fields. They clamp on insert. Prefer validation plus
`u32_as_i32` / `u64_as_i64`, or widen the SQL column.

Manual impls remain valid for sidecars with child tables, computed
columns, multi-row inserts, or custom validation:

```rust
impl PgMemorySidecar for DocumentFiledV1 {
    fn insert_memory_sidecar<'t>(
        &'t self,
        tx: &'t mut sqlx::Transaction<'_, sqlx::Postgres>,
        memory_id: MemoryId,
    ) -> PgSidecarFuture<'t> {
        Box::pin(async move {
            sqlx::query(
                "INSERT INTO my_flavor.document_filed_v1
                    (memory_id, source_path, title)
                 VALUES ($1, $2, $3)",
            )
            .bind(memory_id.into_inner())
            .bind(&self.source_path)
            .bind(&self.title)
            .execute(tx.as_mut())
            .await
            .map_err(|err| StorageError::Internal(err.to_string()))?;
            Ok(())
        })
    }
}

impl PgMemoryPayload for DocumentFiledV1 {
    fn load_memory_payload(
        ctx: PgSidecarReadCtx<'_>,
        memory_id: MemoryId,
    ) -> PgMemoryPayloadFuture<'_> {
        Box::pin(async move {
            let row: Option<(String, String)> = ctx
                .fetch_optional_by_memory_id(
                    "SELECT source_path, title
                       FROM my_flavor.document_filed_v1
                      WHERE memory_id = $1",
                    memory_id,
                )
                .await?;
            Ok(row.map(|(source_path, title)| {
                SidecarPayload::fact(DocumentFiledV1 { source_path, title })
            }))
        })
    }
}
```

`PgSidecarReadCtx` never exposes `sqlx::PgPool`; manual readback may use
its memory-id-bound sidecar query helpers. Do not reference
`proxima_core.*` from sidecar read SQL.

Storage dispatches typed `SidecarPayload`. Protocol adapters serialize
only at MCP/protocol output.

### Extending a substrate Fact

A Fact write carries a *slice* of sidecar payloads, not one. The
substrate owns the Fact and its own sidecar; a flavor may add further
rows against the same `memory_id` — extra columns on an event the
substrate defines, without owning the event. `core/upload-v1` is the
worked example (docs/11 §The upload Fact): core records that a file
arrived, and a flavor records what it intends to do with it.

Register the extension schema and its PG sidecar exactly as you would
your own, then pass its payload alongside the substrate's:

```rust
engine
    .complete_upload_as_fact(blobs, &authz, owner, &upload_id, &[
        SidecarPayload::fact(AcmeIngestQueuedV1 { queue: "ocr".into() }),
    ])
    .await?;
```

What the mechanism guarantees, and why it is data rather than a callback:

- **Destination is resolved per payload.** Storage routes each payload by
  its own `(kind, schema_id, schema_version)` through the sidecar
  registry; an unregistered schema is a `ConstraintViolation`. Two
  payloads cannot be transposed into each other's tables by an ordering
  slip, and a flavor cannot name a destination it has not registered.
- **All or nothing.** Every payload lands in the Fact's transaction; one
  failure rolls back the Fact and the substrate's own sidecar with it.
- **Add only.** Storage keeps sole authority over the transaction, so an
  extension can only insert rows it registered. There is no handle to the
  Fact row and none to misuse.
- **Re-entrant.** Payloads are data, so the bounded retry around
  begin→body→commit rebuilds them on every attempt; a closure could not
  be re-run.

The Fact's own identity is not extensible: `fact_sidecar_table` and its
natural-key columns still come from the Fact's registered schema, because
a stateful Fact's identity belongs to the event, not to whatever a flavor
stapled onto it.

A schema that declares a sidecar table with no PG sidecar registered is
refused at boot, not at write time — so the failure a flavor can actually
reach is a registered sidecar whose migration never ran.

## Typed Citations

Use the typed inline path when the cited artefact or mapping has
schema-owned fields.

Payloads:

```rust
impl CitedObjectPayload for UploadedBlobV1 {
    const SCHEMA_ID: &'static str = "acme/uploaded-blob-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        "acme.cited_uploaded_blob_v1"
    }

    fn idempotency_key(&self) -> [u8; 32] {
        self.blake3
    }
}

impl CitationMappingPayload for BlobByteRangeV1 {
    const SCHEMA_ID: &'static str = "acme/blob-byte-range-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> Option<&'static str> {
        Some("acme.citation_blob_byte_range_v1")
    }

    fn cited_object_schema() -> SchemaId {
        UploadedBlobV1::schema_id()
    }
}
```

Invocation:

```rust
// `from_payload` carries no owner; the engine stamps it from authorization.
let draft = FactWriteCommand::from_payload(
    "acme/importer",
    source_batch_id,
    &fact_payload,
    observed_at,
);
let cited_object = InlineCitedObjectDraft {
    schema_id: UploadedBlobV1::schema_id(),
    schema_version: SchemaVersion::new(UploadedBlobV1::SCHEMA_VERSION),
    payload_bytes: canonical_json_bytes(&serde_json::to_value(&object_payload)?),
};
let mapping = InlineCitationMappingDraft {
    schema_id: BlobByteRangeV1::schema_id(),
    schema_version: SchemaVersion::new(BlobByteRangeV1::SCHEMA_VERSION),
    payload_bytes: canonical_json_bytes(&serde_json::to_value(&mapping_payload)?),
};

let authorized = engine
    .authorize_fact_with_citation(&authz, Relation::Ingest, draft, cited_object, mapping)
    .await?;
engine
    .ingest_fact_with_citation_and_typed_sidecar(
        &authorized,
        &SidecarPayload::fact(fact_payload.clone()),
        embedding_model_id,
    )
    .await?;
```

The SDK surface receives Engine admission witnesses and typed sidecar
contexts. It never receives `sqlx::PgPool`; backend adapters keep the
pool private.

Stateful Fact ingest resolves the series handle from
`FactPayload::natural_key_columns()` when `handle` is unset. A/P series
continuity is `Engine::owned_series_handle` (sidecar columns, owner-only)
or `supersedes`. Flavor `src/` does not JOIN `proxima_core.memory_head`.
Goal assignment / evidence are `GoalRow` fields; filter with
`QueryRequest::assignment` / `evidence_contains`.

Typed path guarantees:

| Check | Enforced by |
|---|---|
| cited-object schema exists and decodes | `authorize_fact_with_citation` |
| mapping schema exists and decodes | `authorize_fact_with_citation` |
| mapping targets the cited-object schema | `CitationMappingPayload::cited_object_schema()` |
| cited object has a typed sidecar | engine authorization |
| Fact row, citation rows, and sidecars commit atomically | PG ingest helper |

Opaque `CitationSpec` is for content-addressed cited objects with no
typed sidecar payload and pure-link mappings. Do not copy it for
domain documents, byte ranges, page spans, media boxes, or chat
messages; use typed `InlineCitedObjectDraft` +
`InlineCitationMappingDraft`.

## Registry

`src/lib.rs` (in-repo witness: `flavors/code/src/lib.rs`):

```rust
proxima::flavor::proxima_flavor! {
    name = "my-flavor",
    display_name = "My Flavor",
    fact_schemas = [DocumentFiledV1],
    abstraction_schemas = [],
    perspective_schemas = [],
    goal_schemas = [],
    mcp_tools = [],
}
```

Macro keys and prefix guards: see 08 §Macro Surface.

Register every schema exactly once. There is no `relations` or
`edge_schemas` key to fill in — see §Declaring references below.

## FlavorBundle

One public bundle type per flavor (in-repo: `CodeFlavor` in `flavors/code/src/lib.rs`):

```rust
pub struct MyFlavor;

impl FlavorBundle for MyFlavor {
    fn register(registry: &mut FlavorRegistry) -> Result<(), FlavorRegistryError> {
        self::register(registry)
    }

    fn register_pg_sidecars(registry: &mut proxima::flavor::PgSidecarRegistry) {
        registry.add_fact::<DocumentFiledV1>();
    }

    fn migrators() -> Vec<NamedMigrator> {
        vec![NamedMigrator::new("my-flavor", migrator())]
    }
}
```

Composite app:

```rust
type LinkedFlavors = (proxima_code::CodeFlavor,);

impl FlavorBundle for HostApp {
    fn register(registry: &mut FlavorRegistry) -> Result<(), FlavorRegistryError> {
        <LinkedFlavors as FlavorBundle>::register(registry)
    }

    fn register_pg_sidecars(registry: &mut PgSidecarRegistry) {
        <LinkedFlavors as FlavorBundle>::register_pg_sidecars(registry);
    }

    fn migrators() -> Vec<NamedMigrator> {
        <LinkedFlavors as FlavorBundle>::migrators()
    }
}
```

Consumers call the bundle surface. They do not manually coordinate
`register`, `register_pg_sidecars`, and `freeze_against`.

## Deriving Abstractions

Declaring an `AbstractionPayload` gives a flavor a derived-memory schema;
writing one goes through `Engine::author_derived_authorized`. The
request names the operator that produced the memory, the text it is
embedded from, its typed sidecar, and what it was derived from.

Note what the request does *not* contain: a relation, an authorship kind, or
an edge kind. `derived_from` names targets only; the engine writes one
`origin` index row per entry, in this write's own transaction, because that
is what a derivation declaration *means*.

```rust
let derived_from = [EdgeEndpoint::memory(EntityKind::Fact, source_fact_id)];

ctx.engine.author_derived_authorized(&authz, AuthorDerivedRequestInput {
    memory_id: derived_id,
    owner,
    kind: EntityKind::Abstraction,
    text: rendered,                       // this is what gets embedded
    schema_id: MySlice::schema_id(),
    schema_version: SchemaVersion::new(MySlice::SCHEMA_VERSION),
    operator_kind: MemoryOperatorKind::FtoA,
    operator_id, input_contract_id,
    source_batch_id: None,
    model_id: "my-flavor/slicer-v1",
    prompt_version: "1",
    sidecar_payload: SidecarPayload::abstraction(payload),
    authoring_perspective_id: None,
    derived_from: &derived_from,
    supersedes: None,
    lexical_language: None,
}).await?;
```

The outcome reports an `edge_count`, not a list of handles: an edge has no id
to hand back, and re-running the write re-asserts the same rows.

Four contract points that are easy to get wrong:

- **The sidecar is mandatory.** `AbstractionPayload::sidecar_table()`
  returns `&'static str`, not `Option` — unlike a Fact, a derived memory
  always has a typed sidecar, so declaring one always means owning a
  migration for it.
- **Derive `memory_id` deterministically** (a UUIDv5 over the operator
  identity plus the source memory and slice index, as `flavors/code`
  does) so re-running the operator replays onto the same row instead of
  appending a duplicate. Use `supersedes` when the new output genuinely
  replaces an earlier one; that records the lineage pointers
  (`supersedes` on the new row, `superseded_by` on the old one) in the same
  transaction. Supersession is never an edge — it is the same thing
  persisting through revision.
- **Embedding is synchronous here, but a refused text is not a lost
  write.** The engine embeds `text` inside the write. When the provider
  refuses that text — or dies on it — and still answers a liveness probe,
  the memory lands with no vector and a durable `embedding_jobs` row
  enqueued in the same transaction, exactly the path a Fact always takes,
  and the outcome's `embedding_deferred` says so. The memory is lexically
  findable immediately and semantically findable once a drain runs (which
  is also what bisects an over-limit text into chunks). Only a provider
  that is genuinely unavailable fails the write. A flavor deriving many
  memories should still checkpoint per output, not per batch.
- **`text` is the whole semantic surface.** `render()` / authored text
  is the only string ever embedded; `search_projection()` adds lexical
  reach over sidecar columns but never affects the vector.
- **Scoping a search takes a projection, not a copy of the text.** A tag
  filter is the only predicate that narrows `core_search_memories` to
  part of a corpus — `schema_id` is exact-match and there is no
  per-column filter — and the unscoped branch carries no tags, so
  a tag-filtered query is served by projection branches alone. Declare a
  `tags text[]` column, name it as `tag_column`, and project the memory's
  own text with `SearchProjectionField::MEMORY_TEXT`:

  ```rust
  fn search_projection() -> Option<SearchProjection> {
      Some(SearchProjection {
          fields: &[SearchProjectionField::MEMORY_TEXT],
          tag_column: Some("tags".to_owned()),
          tsv_column: None,
          language_column: None,
      })
  }
  ```

  Do not copy rendered text into the sidecar to achieve this. The copy
  is a second corpus that must stay byte-identical forever, and the day
  it drifts a scoped and an unscoped search return different text for
  one memory. Add sidecar fields alongside `MEMORY_TEXT` when the sidecar
  genuinely holds searchable content the render does not.

## Background Workers

`FlavorBundle::spawn_workers(&FlavorWorkerContext) -> Vec<FlavorWorker>`
(default: empty) lets a flavor contribute durable background workers —
e.g. a document-ingestion flavor driving OCR jobs. The serving runtime
(`Proxima::run`) calls it once after boot; tuple bundles chain element
workers in tuple order, and `RunningProxima::shutdown()` cancels and
joins every worker. `FlavorWorkerContext` carries the engine, a
`CancellationToken` that observes the runtime's shutdown (a child of
the runtime's own token — cancelling it does not shut the runtime
down), plus the app's composed `FlavorServices`; each worker MUST
terminate when that token is cancelled (select on `cancel.cancelled()`
in the work loop, mirroring the core embedding worker). A panicking
worker never takes the host down — its join error is logged at shutdown. The serverless
`Proxima::build` variant spawns no workers; hosts driving a
`BuiltProxima` own their own background tasks.

`ctx.service::<CitedBlobService>()` and
`ctx.service::<CitedBlobReadService>()` resolve disjoint capabilities over
the same host-wired backend as `core_upload`. Both are absent unless the host
configured S3 (see [10-configuration.md](10-configuration.md) §Large
Artefact S3), so a worker that needs one should fail its job typed rather than
no-op into a silently idle loop.

A queued delegated worker persists only `DelegationId`, resolves
`ctx.service::<DelegatedAuthorityService>()`, and redeems `DelegatedPhase` at
job claim and at each subsequent phase boundary. It passes `&DelegatedPhase` and the exact
`OwnerRef` to the explicitly delegated-capable Engine/blob service methods;
it never reconstructs a raw delegated `AuthzContext`. Ordinary authenticated
jobs continue to pass `&AuthzContext`. `CitedBlobService::read_url` answers a
presigned URL. `CitedBlobReadService::collect_verified` additionally requires
a `NonZeroU64` ceiling, and no bytes return until stored length, BLAKE3, and
SHA-256 all match. Neither outcome exposes bucket/object key. Owner
reconciliation is not delegated-capable.

To unit-test a `spawn_workers` implementation without booting the
runtime, build the context with
`FlavorWorkerContext::new_for_tests(engine, cancel)` (available under
`cfg(test)`, the `testkit` feature, or debug builds). Attach the exact
test service set with
`.with_services(FlavorServices::with(CitedBlobService::new(Arc::new(MyFake))))`.

## Migrations

```rust
#[must_use]
pub fn migrator() -> sqlx::migrate::Migrator {
    let mut migrator = sqlx::migrate!("./migrations");
    migrator.set_ignore_missing(true);
    migrator
}
```

Rules:

1. SQLx migration versions share one database-global namespace.
2. Core migrator runs before flavor migrators.
3. Every flavor migrator sets `ignore_missing(true)`.
4. `run_core_and_flavor_migrations` rejects duplicate versions before any
   database write; external migrator composition owns the same collision
   check if it bypasses this facade.
5. Pre-v1 flavor schema changes may be squashed only before persisted
   compatibility matters.

Version lanes:

| Source | Reserved versions |
|---|---|
| Proxima core | `0001_v008.sql` (version 1) |
| example/host migrators | timestamp versions ending `00..=19` |
| first-party flavors | timestamp versions ending `20..=39` |
| downstream host composition | timestamp versions ending `60..=99`; external hosts own collision avoidance when they compose migrators outside Proxima's facade |

Run `python3 scripts/check-migration-ranges.py` before adding a migration.

## MCP Tools

Tool contract:

| Field | Rule |
|---|---|
| Name | provider-safe `<flavor_id>_<verb>` |
| Args | `Deserialize + JsonSchema` |
| Output | `Serialize` |
| Context | `ToolCtx`: Owner, AuthzContext, frozen registry, optional `ToolCaller`, optional caller Self Perspective, optional Engine, typed ToolServices |
| Storage | use typed engine/storage APIs and flavor services; no public raw `PgPool` capability |
| Writes | emit typed Facts / A/P / Goals through registered schemas; no tool writes an edge |

MCP JSON is protocol boundary only. Flavor SDK tool code targets `Tool`;
MCP is an adapter projection.

Caller provenance is invocation context, not a service:

```rust
let caller = ctx.caller().ok_or_else(|| ToolError::Other("caller metadata required".into()))?;
let model_id = caller.model_id.as_str();
```

`ToolCaller` carries `model_id`, `client_name`, and `client_version`. MCP and
REST adapters populate it; direct `ToolCtx::new` calls leave it absent unless
the host adds `.with_caller(Some(...))`. The optional
`ctx.caller_self_perspective()` stays separate.

Paged reads: call `proxima::flavor::reject_zero_limit(args.limit)?` before
anything else, then clamp the upper bound however the tool likes. The two
ends are not symmetric — a limit above the maximum still answers the
caller's intent, `limit: 0` cannot. Answering it yields either a
well-formed empty page indistinguishable from "nothing matched" or a page
of one that answers a question nobody asked, and the engine has rejected
`limit == 0` from the start. The helper is shared rather than reimplemented
because the in-tree tools proved that three implementations means three
behaviours.

### Host-owned dependencies: the service seam

Core cannot name a flavor's own service types, so anything a tool needs
beyond the engine travels through `FlavorServices` — a concrete-type-keyed
map assembled once at boot and shared with MCP tools, the REST projection,
and flavor workers. This is how `core_upload` finds its `CitedBlobService`,
and how a flavor's tools and workers reach their own sidecar tables.

The host half is a fallible `FlavorApp::services` override:

```rust
fn services(ctx: &AppContext) -> Result<FlavorServices, FlavorServiceError> {
    let mut services = FlavorServices::default();
    services.try_insert(MyFlavorStore::from_backend_pool_for_host(
        ctx.clone_pool_for_host(),
    ))?;
    Ok(services)
}
```

Tools and workers resolve by type and must handle absence rather than assume
the host wired it:

```rust
let Some(store) = ctx.service::<MyFlavorStore>() else {
    return Err(McpToolError::new(
        McpToolErrorKind::Internal,
        "host did not wire MyFlavorStore",
    ));
};
```

Two things to note. `AppContext::clone_pool_for_host` is the only route
to a `PgPool`, and it is deliberately kept off the supported export tier:
wrap it in a store type that keeps `proxima_core.*` SQL private, exactly
as `proxima-code` does, rather than passing the pool around. Tuple apps fold
service sets left-to-right; `(A,)` is the identity-preserving singleton form.
Duplicate concrete types fail boot with `FlavorServiceError::DuplicateService`
instead of silently overriding an earlier flavor or the substrate's service.

When S3 is configured, the runtime appends three substrate-owned entries over
one shared backend: `CitedBlobService` for presigned upload/read,
`CitedBlobReadService` for bounded verified bytes, and
`CitedBlobOwnerReconcileService` for an authorized, report-only integrity
check. A tool passes `&ctx.authz` and `ctx.owner` to `reconcile_owner`; the
service re-checks Fact-read authority before Postgres or S3 access and returns
counts plus missing cited-object ids. Bucket names, object keys, and
orphan/foreign locator samples are absent. Global bucket reconciliation is
not an extension service and requires the host-held `SystemAuthority`.

## Tests

Minimum:

| Test | Assertion |
|---|---|
| Registry | all schemas and tools registered and prefixed |
| Sidecar insert | typed payload writes memory row + sidecar row atomically |
| Sidecar load | query/open returns typed payload projection |
| Migration | fresh DB has every sidecar table/enum/index |
| Idempotency | same key replays; changed semantic key inserts |
| Search projection | only schema-declared columns indexed |
| References | `references()` yields one index row per declaration, and a replay re-asserts the same rows |
| MCP tool | tool emits/reads typed payloads |

Use `flavors/code` for Fact/A/P/reference/MCP coverage and
`apps/proxima-mcp` for the host that serves it.

## Verification

Smallest relevant check:

```sh
cargo fmt --all
cargo check --workspace
cargo clippy --workspace --all-targets
cargo nextest run -E 'package(<flavor-crate>)'
```

Before done:

```sh
rg -n "serde_json::Value|jsonb|row_to_json|canonical_payload|canonical_payload_bytes" \
  flavors/<name> crates/core/src crates/storage-pg/src
```

Allowed hits: MCP/protocol args, schema JSON, and tests for protocol
serialization. Internal identity/storage/query paths must stay typed.

## Done Checklist

- `FlavorBundle` exported.
- `proxima_flavor!` registration complete.
- Payload keys are explicit and schema-owned.
- Sidecar SQL exists for every registered sidecar table.
- PG sidecar insert/load registered.
- Migrator exported with `ignore_missing(true)`.
- No JSON escape hatch in payload structs or sidecars.
- Prefix guards pass at registry freeze.
- `cargo clippy --workspace --all-targets` clean.
