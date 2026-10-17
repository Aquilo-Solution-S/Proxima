# Build a flavor
> Status: current.

A flavor is a vocabulary crate linked into a host. It contributes typed
payloads, references, sidecars, tools, and migrations. It uses the `proxima`
facade; it neither runs as a remote plugin nor accesses core tables directly.

## Walk through the executable example

The facade's
[`flavor_kit_pg.rs`](https://github.com/Aquilo-Solution-S/Proxima/blob/main/crates/proxima/tests/flavor_kit_pg.rs)
is a complete small flavor and host. It uses the same public SDK as an external
crate. Follow its `KitNoteV1`, sidecar, `KIT_CONTRACT`, migration, and `kit`
bundle in that order, then run it against your owned development test server:

```sh
cargo test -p proxima --test flavor_kit_pg
```

The fixture verifies typed sidecar persistence, split-role boot, RLS
classification, migration ledgers, and registry behavior. The larger production
example is
[`flavors/code`](https://github.com/Aquilo-Solution-S/Proxima/tree/main/flavors/code).
Build the API reference locally with `cargo doc -p proxima --no-deps --open`;
`proxima::flavor` is the SDK entry point.

## 1. Define the vocabulary

Choose a flavor prefix and typed payload kind. Facts are observed admissions;
Abstractions are declared derivations; Perspectives are interpretations;
Goals are intent. Define validation, rendering, replay identity, and references
on the corresponding payload trait. A Fact's receipt key uses schema-owned
semantic bytes, not arbitrary JSON serialization.

For example, a new document observation's payload can be:

```rust
use proxima::flavor::{FactPayload, PayloadKeyBuilder};
use serde::{Deserialize, Serialize};

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

Implement `references()` when the payload carries node references. Ingest
resolves the targets and derives pins atomically. A flavor defines neither an
edge-writing verb nor a new pin kind. See [concepts](concepts.md#pins).

## 2. Persist the typed payload

Add a migration in the flavor's migration directory. Its sidecar has the
admission key and the payload's typed columns; table shape stays in that
migration. Declare every table's owner-RLS class using the core installer and
assert the schema census. The example migration in `flavor_kit_pg.rs` shows the
installer; released Code migrations show the production lane.

For a one-row payload, the typed helper connects fields to the sidecar:

```rust
proxima::flavor::pg_sidecar! {
    payload: DocumentFiledV1,
    row: DocumentFiledRow,
    kinds: [Fact],
    table: "my_flavor.document_filed_v1",
    key: t,
    fields: {
        source_path => source_path: (text),
        title => title: (text),
    },
}
```

Use `PgMemorySidecar` directly for computed or multi-row sidecars. Authorized
helpers and private permits bind reads/writes to admitted rows; do not query
`proxima_core` from flavor SQL. A generic JSON column is not a typed sidecar.

## 3. Declare and register the contract

Follow `KIT_CONTRACT` in the executable example. Each schema declares kind,
sidecar, search projection, embedding recipe, provenance, and transfer behavior.
Each stored surface declares erase/export/transfer behavior. A surface that is
not searchable or embeddable says so explicitly with its reason. See
[data and erasure](data-and-erasure.md#declared-inverses).

Core search includes every declared projection with comparable score bands and
a supported projection renderer, both unscoped and when selected by schema.
Tags narrow those results; they are not needed to reach a flavor's projection.
A tag-filtered query requires the projection to declare its tag column.

Use `proxima::flavor_bundle!` to register the payloads, contract, sidecars, and
`NamedMigrator` together. Its rustdoc shows all parameters. Use a manual
`FlavorBundle` implementation only when composition needs it. Registry freeze
refuses inconsistent declarations; migrations and the PG catalog tests verify
the physical half. Neither check replaces the other.

Add tools under the flavor prefix. A handler implements the typed `Tool`
contract, receives `ToolCtx`, and resolves host-wired services. Declare the
actual effect once per action; owner scope, MCP hints, REST methods, and OpenAPI
consume that declaration. A tool has no raw database pool from the SDK.

## 4. Link into the host

Implement the host's `FlavorApp` or compose tuple bundles at build time. Register
host services through `FlavorApp::services` and configure remote stores through
`OtherStores`; see [host integration](host-integration.md). Keep the same
Proxima git selector across all crates in the consumer graph. Cargo package
versions are not published release identifiers.

## Acceptance checklist

| Check | Enforcer |
|---|---|
| Typed identity, references, and field validation | Payload trait tests |
| Schema/sidecar/tool/inverse declarations agree | Registry freeze and conformance tests |
| Columns, triggers, owner RLS, and constraints agree | Migration and real PG tests |
| Old released migrations unchanged and new versions above the released head | Migration gate |
| Admission, sidecar, and publication rollback together | Transaction tests |
| Erase/export/transfer cover new storage | Inverse and lifecycle tests |
| Tools appear under the intended host profile | Served MCP catalog and host e2e tests |
| Public signatures usable from the facade alone | Consumer compile check and rustdoc |

Run the workspace gates in [contributing](contributing.md#local-verification).
Inspect the live catalog after starting the host; static registration alone
does not prove the served profile exposes the capability.
