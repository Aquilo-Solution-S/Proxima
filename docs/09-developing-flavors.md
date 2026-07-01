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
let draft = FactWriteCommand::from_payload(
    owner,
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

let authorized = engine.authorize_fact_with_citation(
    authz,
    Relation::Ingest,
    draft,
    cited_object,
    mapping,
)?;
let fact_sidecar = fact_payload.clone();
engine
    .ingest_authorized_fact_with_sidecar(
        authz,
        authorized,
        embedding_model_id,
        move |tx, outcome| {
            Box::pin(async move {
                fact_sidecar
                    .insert_memory_sidecar(tx, outcome.memory_id)
                    .await
            })
        },
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
4. Pre-v1 flavor schema changes may be squashed only before persisted
   compatibility matters.

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
