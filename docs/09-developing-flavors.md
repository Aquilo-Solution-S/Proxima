# 09. Developing Flavors

> New to flavor authoring? Start with [tutorials/build-first-flavor.md](tutorials/build-first-flavor.md), then return here for the complete checklist.

## Contract

Flavor = build-time vocabulary crate.

| Owns | Examples |
|---|---|
| Payload schemas | `FactPayload`, `AbstractionPayload`, `PerspectivePayload`, `GoalPayload`, `EdgePayload` |
| Sidecar storage | SQL tables + typed PG insert/load impls |
| Relations | `RelationDescriptor` values under the flavor prefix |
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

Out-of-tree flavor:

```
src/flavor.rs
migrations/
```

Reference: `examples/embedded-minimal/src/flavor.rs`.

## Build Order

1. Pick one stable `flavor_id`.
2. Define typed payload structs.
3. Implement payload traits and schema-owned keys.
4. Write sidecar SQL tables.
5. Implement PG sidecar insert/load traits.
6. Register schemas/relations/tools with `proxima_flavor!`.
7. Wrap the flavor in `FlavorBundle`.
8. Add ingestion/operators/tools.
9. Add tests.
10. Run workspace verification.

## Namespace

| Item | Rule |
|---|---|
| Flavor id | `kebab-case`; stable persisted prefix |
| Schema ids | `<flavor_id>/<local-schema-vN>` |
| Relation ids | `<flavor_id>/<local-relation>` |
| MCP tools | provider-safe `<flavor_id>_<tool>` |
| SQL schema | `<flavor_id>` converted to snake case |
| Rust crate | `proxima-<local>` for in-repo first-party flavors |

`proxima_schema_id!("x")` derives `CARGO_PKG_NAME + "/x"`. Use literal
ids when crate name and flavor id intentionally differ.

## Payload Traits

Facts:

```rust
impl FactPayload for DocumentFiledV1 {
    const SCHEMA_ID: &'static str = "embedded-minimal/document-filed-v1";
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
        Some("embedded_minimal.document_filed_v1")
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
| Edge | typed sidecar only when relation descriptor has payload schema |

No `serde_json::Value` payload fields. No generic canonical payload
encoder. Keys are schema-owned semantic identity bytes, built with
`PayloadKeyBuilder`.

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
the stable request id, and writes the required Self assignment edge; host
apps do not insert `proxima_core.goals` rows directly.

Include `SCHEMA_ID` and `SCHEMA_VERSION` through `PayloadKeyBuilder::new`.
Never derive keys from arbitrary JSON serialization.

## Sidecar Tables

One sidecar-backed payload schema maps to one sidecar table.

```sql
CREATE SCHEMA IF NOT EXISTS embedded_minimal;

CREATE TABLE embedded_minimal.document_filed_v1 (
  memory_id uuid PRIMARY KEY
    REFERENCES proxima_core.memories(memory_id),
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
    table: "embedded_minimal.document_filed_v1",
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
                "INSERT INTO embedded_minimal.document_filed_v1
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
                       FROM embedded_minimal.document_filed_v1
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

`src/lib.rs`:

```rust
proxima::flavor::proxima_flavor! {
    name = "embedded-minimal",
    display_name = "Embedded Minimal Example",
    fact_schemas = [DocumentFiledV1],
    abstraction_schemas = [],
    perspective_schemas = [],
    goal_schemas = [],
    edge_schemas = [],
    relations = [],
    mcp_tools = [],
}
```

Macro keys and prefix guards: see 08 §Macro Surface.

Register every schema exactly once. Register every typed relation payload
as an `edge_schemas` entry and point the relation descriptor at that
schema.

## FlavorBundle

One public bundle type per flavor:

```rust
pub struct EmbeddedMinimalFlavor;

impl FlavorBundle for EmbeddedMinimalFlavor {
    fn register(registry: &mut FlavorRegistry) -> Result<(), FlavorRegistryError> {
        self::register(registry)
    }

    fn register_pg_sidecars(registry: &mut proxima::flavor::PgSidecarRegistry) {
        registry.add_fact::<DocumentFiledV1>();
    }

    fn migrators() -> Vec<NamedMigrator> {
        vec![NamedMigrator::new("embedded-minimal", migrator())]
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
embedded from, its typed sidecar, and the provenance edges back to the
Facts it was derived from:

```rust
let relation = ctx.engine.registry()
    .resolve_relation(CORE_DERIVED_FROM_RELATION)
    .expect("core/derived-from is a substrate relation");

let edges = [AuthorDerivedEdgeInput {
    relation,
    source_kind: EntityKind::Abstraction,
    source_memory_id: derived_id,
    target_kind: EntityKind::Fact,
    target_memory_id: source_fact_id,
    authorship_kind: EdgeAuthorshipKind::OperatorFtoA,
    authorship_owner_memory_id: None,
}];

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
    supersedes: None,
    lexical_language: None,
    edges: &edges,
}).await?;
```

Four contract points that are easy to get wrong:

- **The sidecar is mandatory.** `AbstractionPayload::sidecar_table()`
  returns `&'static str`, not `Option` — unlike a Fact, a derived memory
  always has a typed sidecar, so declaring one always means owning a
  migration for it.
- **Derive `memory_id` deterministically** (a UUIDv5 over the operator
  identity plus the source memory and slice index, as `flavors/code`
  does) so re-running the operator replays onto the same row instead of
  appending a duplicate. Use `supersedes` when the new output genuinely
  replaces an earlier one; that also writes a `core/supersedes` edge in
  the same transaction.
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
- **`text` is the whole semantic surface.** It lands verbatim in
  `memories.text`, which is the only string ever embedded;
  `search_projection()` adds lexical reach over sidecar columns but never
  affects the vector.
- **Scoping a search takes a projection, not a copy of the text.** A tag
  filter is the only predicate that narrows `core_search_memories` to
  part of a corpus — `schema_id` is exact-match and there is no
  per-column filter — and the base `memories` branch carries no tags, so
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

  Do not copy `memories.text` into the sidecar to achieve this. The copy
  is a second corpus that must stay byte-identical forever, and the day
  it drifts a scoped and an unscoped search return different text for
  one memory. Exactly this projection — the single `MEMORY_TEXT` field,
  no `language_column` — also reads the stored `memories.search_tsv`
  rather than tokenising each candidate row, so the sidecar needs no
  tsvector column and no GIN index on text, only on `tags`. Add sidecar
  fields alongside it when the sidecar genuinely holds searchable
  content the memory text does not.

## Background Workers

`FlavorBundle::spawn_workers(&FlavorWorkerContext) -> Vec<FlavorWorker>`
(default: empty) lets a flavor contribute durable background workers —
e.g. a document-ingestion flavor driving OCR jobs. The serving runtime
(`Proxima::run`) calls it once after boot; tuple bundles chain element
workers in tuple order, and `RunningProxima::shutdown()` cancels and
joins every worker. `FlavorWorkerContext` carries the engine, a
`CancellationToken` that observes the runtime's shutdown (a child of
the runtime's own token — cancelling it does not shut the runtime
down), and `blobs`; each worker MUST terminate when that token is
cancelled (select on `cancel.cancelled()` in the work loop, mirroring
the core embedding worker). A panicking worker never takes the host
down — its join error is logged at shutdown. The serverless
`Proxima::build` variant spawns no workers; hosts driving a
`BuiltProxima` own their own background tasks.

`blobs` is the host-wired cited-blob lane — the same `CitedBlobService`
`core_upload` resolves from its MCP tool extensions, re-exported from
`proxima::flavor` along with `CitedBlobPort`. It is `Some` only when
the host configured S3 (see
[10-configuration.md](10-configuration.md) §Large Artefact S3), so a
worker that needs it should fail its job typed
rather than no-op into a silently idle loop. Unlike an MCP tool, a
worker has no request to inherit authority from: every port method
takes an `AuthzContext` and an `OwnerRef` that the worker supplies per
job, normally from the job row that its tool wrote when the upload
landed. `AuthzContext::single_owner` covers personal owners only — it
returns a denied context for a group owner, where
`AuthzContext::for_subject_with_role` is the right mint. `read_url`
answers a presigned URL, never the bucket or object key, so a worker
that needs the bytes fetches them itself over HTTP.

To unit-test a `spawn_workers` implementation without booting the
runtime, build the context with
`FlavorWorkerContext::new_for_tests(engine, cancel)` (available under
`cfg(test)`, the `testkit` feature, or debug builds; it needs a Tokio
context, so call it from `#[tokio::test]`). Attach a fake blob service
to it with `.with_blobs(CitedBlobService(Arc::new(MyFake)))`.

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
| Proxima core | `1..=9999`; `2..=7` retired pre-v0.0.4 rows |
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
| Context | `ToolCtx`: Owner, AuthzContext, frozen registry, optional Engine, typed ToolServices |
| Storage | use typed engine/storage APIs and flavor services; no public raw `PgPool` capability |
| Writes | emit typed Facts / A/P / Goals / Edges through registered schemas |

MCP JSON is protocol boundary only. Flavor SDK tool code targets `Tool`;
MCP is an adapter projection.

Paged reads: call `proxima::flavor::reject_zero_limit(args.limit)?` before
anything else, then clamp the upper bound however the tool likes. The two
ends are not symmetric — a limit above the maximum still answers the
caller's intent, `limit: 0` cannot. Answering it yields either a
well-formed empty page indistinguishable from "nothing matched" or a page
of one that answers a question nobody asked, and the engine has rejected
`limit == 0` from the start. The helper is shared rather than reimplemented
because the in-tree tools proved that three implementations means three
behaviours.

### Host-owned dependencies: the extension seam

Core cannot name a flavor's own service types, so anything a tool needs
beyond the engine travels through `McpToolExtensions` — a `TypeId`-keyed
map the host fills and the tool reads back. This is how `core_upload`
finds its `CitedBlobService`, and how a flavor's tools reach their own
sidecar tables.

The host half is a `FlavorApp::mcp_tool_extensions` override:

```rust
fn mcp_tool_extensions(ctx: &AppContext) -> McpToolExtensions {
    let mut extensions = McpToolExtensions::default();
    extensions.insert(MyFlavorStore::from_backend_pool_for_host(
        ctx.clone_pool_for_host(),
    ));
    extensions
}
```

The tool half resolves it by type, and must handle absence rather than
assume it:

```rust
let Some(store) = ctx.extensions.get::<MyFlavorStore>() else {
    return Err(McpToolError::new(
        McpToolErrorKind::Internal,
        "host did not wire MyFlavorStore",
    ));
};
```

Two things to note. `AppContext::clone_pool_for_host` is the only route
to a `PgPool`, and it is deliberately kept off the supported export tier:
wrap it in a store type that keeps `proxima_core.*` SQL private, exactly
as `proxima-code` does, rather than passing the pool around. And the
runtime calls
`mcp_tool_extensions` *before* `spawn_workers`, so a service built here
can also be handed to a worker.

## Tests

Minimum:

| Test | Assertion |
|---|---|
| Registry | all schemas, relations, tools registered and prefixed |
| Sidecar insert | typed payload writes memory row + sidecar row atomically |
| Sidecar load | query/open returns typed payload projection |
| Migration | fresh DB has every sidecar table/enum/index |
| Idempotency | same key replays; changed semantic key inserts |
| Search projection | only schema-declared columns indexed |
| MCP tool | tool emits/reads typed payloads |

Use `examples/embedded-minimal` for the smallest shape and
`flavors/code` for full Fact/A/P/Edge/MCP coverage.

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
