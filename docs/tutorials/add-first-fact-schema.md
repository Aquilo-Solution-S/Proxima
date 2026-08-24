# Add Your First Fact Schema

## Start From the Code Flavor

The compiling witness is [`flavors/code`](https://github.com/Aquilo-Solution-S/Proxima/blob/main/flavors/code/src/lib.rs):
typed payloads, sidecar tables, `proxima_flavor!`, and a `FlavorBundle`.
The host that serves them is [`apps/proxima-mcp`](https://github.com/Aquilo-Solution-S/Proxima/blob/main/apps/proxima-mcp).

An out-of-tree flavor colocated in a host binary uses the same shape
(`src/flavor.rs` + `migrations/`); there is no second example crate.

## Define the Payload Type

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentFiledV1 {
    pub source_path: String,
    pub title: String,
}

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

The payload does not declare its own search surface — there is no
`search_projection()` method. Searchability is a `SchemaContract` field on
the flavor's `FlavorContract`:

```rust
search: SearchProjectionDecl::Projected {
    fields: &[WeightedField {
        column: "title",
        kind: SearchProjectionColumnKind::Text,
        weight: WEIGHT_UNIFORM,
    }],
    tag_column: None,
    // The row's configuration is the write's; a write naming none is
    // refused. `Pinned("english")` instead fixes it for the surface.
    language: LanguagePolicy::PerRow { column: "lexical_language" },
    bands: BANDS,
    substring: SubstringArm::Off,
},
```

The declared `fields` are copied into `<flavor>.projection` on write and
ranked from there; the sidecar itself is read only to build the snippets of
rows that made the page. A schema that is not a search surface says so —
`SearchProjectionDecl::None { why: "…" }` — and one that declares neither is
refused at freeze. See [09](../09-developing-flavors.md) for the full field
list.

Key rule: the Fact identity key is schema-owned semantic identity bytes, not raw
JSON serialization.

## Add the Sidecar Table

Create or extend the flavor migration:

```sql
CREATE SCHEMA IF NOT EXISTS my_flavor;

CREATE TABLE my_flavor.document_filed_v1 (
  t uuid PRIMARY KEY
    REFERENCES proxima_core.memory(t),
  source_path text NOT NULL,
  title text NOT NULL
);
```

One typed payload schema maps to one sidecar table. Do not add generic `extra`
JSON columns for payload escape hatches.

## Register the Schema

Register the payload in the flavor macro at build time:

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

There is no runtime schema registration endpoint.

## Insert and Load the Sidecar

For simple one-row sidecars, use `proxima::flavor::pg_sidecar!`:

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

Register the sidecar with the bundle:

```rust
impl FlavorBundle for MyFlavor {
    fn register(registry: &mut FlavorRegistry) -> Result<(), FlavorRegistryError> {
        self::register(registry)
    }

    fn register_pg_sidecars(registry: &mut proxima::flavor::PgSidecarRegistry) {
        registry.add_fact::<DocumentFiledV1>();
    }
}
```

Manual `PgMemorySidecar` implementations are still valid for multi-row or
computed sidecars; keep the row type strongly typed either way.

In-repo, that bundle is `CodeFlavor` in `flavors/code/src/lib.rs`, linked by
`apps/proxima-mcp`.

## Verify

```sh
cargo check -p proxima-code
cargo test -p proxima-code
cargo run -p proxima-mcp
```

`flavors/code` has Postgres-backed sidecar tests. If local Postgres is
unavailable, `cargo check -p proxima-code` is the minimum static gate.
Serve and inspect through the host: [Run the MCP Server](run-mcp-server.md).
