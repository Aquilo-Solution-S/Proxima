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

    fn search_projection() -> Option<SearchProjection> {
        Some(SearchProjection {
            fields: &[SearchProjectionField {
                column: "title",
                kind: SearchProjectionColumnKind::Text,
            }],
            tag_column: None,
            // Compute the lexical vector at query time. Set this to the
            // name of a STORED generated column calling the two-argument
            // `proxima_core.lexical_tsv(lexical_language, ...)` once your
            // sidecar migration adds one; see MIGRATING.md,
            // *Flavor SDK changes*.
            tsv_column: None,
            // With a stored vector, also add a `lexical_language regconfig`
            // column mirrored from the owning memories row (attach
            // `proxima_core.sidecar_lexical_language_from_memory` BEFORE
            // INSERT) and name it here, so search ranks each row with the
            // configuration its vector was tokenised with.
            language_column: None,
        })
    }
}
```

Key rule: the Fact identity key is schema-owned semantic identity bytes, not raw
JSON serialization.

## Add the Sidecar Table

Create or extend the flavor migration:

```sql
CREATE SCHEMA IF NOT EXISTS my_flavor;

CREATE TABLE my_flavor.document_filed_v1 (
  memory_id uuid PRIMARY KEY
    REFERENCES proxima_core.memories(memory_id),
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
