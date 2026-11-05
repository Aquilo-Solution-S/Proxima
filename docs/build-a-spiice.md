# Build a spiice
> Status: current.

A spiice is a vocabulary crate linked into a host. It contributes typed
payloads, references, sidecars, tools, and migrations. It uses the `proxiima`
facade; it neither runs as a remote plugin nor accesses core tables directly.

## Walk through the executable example

The test spiice
[`spiices/example`](https://github.com/Aquilo-Solution-S/proxiima/tree/main/spiices/example)
(`proxiima-example`, a test and tutorial fixture, not a product) is a complete
small spiice. It uses the same public SDK as an external crate: among workspace
crates its `Cargo.toml` depends on `proxiima` only. Follow its
`NoteV1` payload and sidecar (`src/payload.rs`), `EXAMPLE_CONTRACT`
(`src/contract.rs`), migration (`migrations/`), tool (`src/tool.rs`) and
`ExampleSpiice` bundle (`src/lib.rs`) in that order. The facade's
[`spiice_kit_pg.rs`](https://github.com/Aquilo-Solution-S/proxiima/blob/main/crates/proxiima/tests/spiice_kit_pg.rs)
composes it as a host and runs it against your owned development test server:

```sh
cargo test -p proxiima --features testkit --test spiice_kit_pg
```

The fixture verifies typed sidecar persistence, split-role boot, RLS
classification, migration ledgers, and registry behavior.
Build the API reference locally with `cargo doc -p proxiima --no-deps --open`;
`proxiima::spiice` is the SDK entry point.

## 1. Define the vocabulary

Choose a spiice prefix and typed payload kind. Facts are observed admissions;
Abstractions are declared derivations; Perspectives are interpretations;
Goals are intent. Define validation, rendering, replay identity, and references
on the corresponding payload trait. Every payload trait has the
`PayloadProjection` supertrait, so implement its typed `KEY_FIELDS`, `display`,
and `durable` projection as well. A Fact's receipt key uses schema-owned
semantic bytes, not arbitrary JSON serialization.

Semantic projections follow the same rule. A payload with a stateful natural
key implements `PayloadProjection` and declares typed `KeyField`s; the frozen
contract derives its `NaturalKeyDeclaration` from that projection. Display and
durable fields are explicit typed accessors. Protocol Serde names do not select
database identity: wire naming and durable identity are separate contracts. A
natural-key value is never recovered by serializing the payload to JSON and
probing field names.

For example, a new document observation's payload can be:

```rust
use proxiima::spiice::{
    DurablePayloadBuilder, FactPayload, KeyField, PayloadDisplay, PayloadKeyBuilder,
    PayloadProjection,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentFiledV1 {
    pub source_path: String,
    pub title: String,
}

impl FactPayload for DocumentFiledV1 {
    const SCHEMA_ID: &'static str = "my-spiice/document-filed-v1";
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
        Some("my_spiice.document_filed_v1")
    }
}

impl PayloadProjection for DocumentFiledV1 {
    const KEY_FIELDS: &'static [KeyField<Self>] = &[
        KeyField::text("source_path", |value: &Self| &value.source_path),
    ];

    fn display(&self) -> PayloadDisplay {
        PayloadDisplay {
            title: Some(self.title.clone()),
            body: Some(self.source_path.clone()),
            ..PayloadDisplay::default()
        }
    }

    fn durable(&self) -> Result<proxiima::spiice::DurablePayload, String> {
        let mut fields = DurablePayloadBuilder::new();
        fields
            .field("source_path", &self.source_path)?
            .field("title", &self.title)?;
        Ok(fields.finish())
    }
}
```

The contract names the same typed projection; stateless schemas use
`NaturalKeyDeclaration::EMPTY`:

```rust
natural_key_columns: NaturalKeyDeclaration::of::<DocumentFiledV1>(),
```

Use `KeyField::padded_text` only when the migration column is PostgreSQL
`CHAR(n)`. It trims database padding before comparison; `KeyField::text`
remains an ordinary text column and does not apply that canonicalization.

Implement `references()` when the payload carries node references. Ingest
resolves the targets and derives pins atomically. A spiice defines neither an
edge-writing verb nor a new pin kind. See [concepts](concepts.md#pins).

## 2. Persist the typed payload

Add a migration in the spiice's migration directory. Its sidecar has the
admission key and the payload's typed columns; table shape stays in that
migration. Declare every table's owner-RLS class using the core installer and
assert the schema census. The example migration in `spiice_kit_pg.rs` shows the
installer; the example spiice's `migrations/` directory shows a lane on disk.

For a one-row payload, the typed helper connects fields to the sidecar:

```rust
proxiima::spiice::pg_sidecar! {
    payload: DocumentFiledV1,
    row: DocumentFiledRow,
    kinds: [Fact],
    table: "my_spiice.document_filed_v1",
    key: t,
    fields: {
        source_path => source_path: (text),
        title => title: (text),
    },
}
```

Omit the owner token when the sidecar has no `owner_id` and reaches its owner
through the memory. Use `owner_pinned: true` when the row stamps `owner_id` at
insert and stays with that owner on transfer (`RetainAtSource`). Use
`owner: follows` when the row stamps `owner_id` at insert (for RLS) but must
move with the memory on transfer (`TransferRule::Follow` with
`Surface.owner_column` set); freeze refuses Follows with RetainAtSource or Keep.
The host derives Follows tables from the frozen PG sidecar registry and installs
their owner RLS and leading `owner_id` indexes around the spiice migration lanes.
A spiice need not classify them in `install_owner_rls`: the host's marks preserve
the Follows policy when that installer runs. The policy checks both the sidecar's
owner and its memory; a forgotten memory hides the row from owner reads while
restore and erase still reach it. Skip-migration boot validates the generated
marks and indexes without DDL. An existing table adopted as Follows must already
have its UUID `owner_id` and memory FK when this host first runs; new tables may
be created by their spiice lane.
On an existing database the first such run supersedes the class your
migration chose for a Follows table and may build its owner index under a
write lock; see [host integration](host-integration.md#boot-a-composed-host)
for the upgrade effects.
Standalone migration jobs and test templates must supply the same frozen
sidecars; see [host integration](host-integration.md#boot-a-composed-host).

Use `PgMemorySidecar` directly for computed or multi-row sidecars. Authorized
helpers and private permits bind reads/writes to admitted rows; do not query
`proxiima_core` from spiice SQL. A generic JSON column is not a typed sidecar.

Installing the frozen sidecars is fallible and asynchronous because storage
checks the registered natural-key declarations against the actual SQL schema:

```rust
let storage = storage.with_sidecars(sidecars).await?;
```

Await and propagate this `Result` during host boot; a sidecar registry is not
installed until its key columns, SQL types, and nullability pass validation.

## 3. Declare and register the contract

For a removed Fact, put a `RetiredFactContract` in the contract's
`retired_facts` slice and remove its type from the active schema lists.
The declaration needs only its `SchemaRef`, the old `Surface` as `sidecar`,
and any additional `inverse_surfaces`; it needs no payload type or PG trait.
Each surface must declare a by-key memory inverse in the spiice's own SQL
namespace (the spiice id with hyphens replaced by underscores). Freeze refuses
active overlap, duplicate declarations, foreign ownership and unsupported
inverse shapes. Use `retired_facts: &[]` when none are declared.

`spiice_bundle!` carries the same contract through ordinary registration.
PG freeze derives `PgRetiredFactSidecar` metadata from it automatically;
do not add a typed sidecar registration for the retired schema. The frozen
core registry's `retired_facts()` and the PG registry's
`retired_fact_sidecars()` expose inverse metadata separately from active
catalogs, readers and writers. `retired_fact_surface(table)` resolves only
declared inverse tables; stored table names alone grant no capability.
Restore refuses a retired schema.

The declaration performs no erase. The lifecycle and rollout order are
defined in the [retired-Fact decision](https://git.aquilo-cloud.com/AQS/uniiverse/src/branch/factory/docs/decisions/2026-11-04-5213-a-retired-fact-is-erased-before-its-schema-is-dropped.md).

Follow `KIT_CONTRACT` in the executable example. Each schema declares kind,
sidecar, search projection, embedding recipe, provenance, and transfer behavior.
Each stored surface declares erase/export/transfer behavior. A surface that is
not searchable or embeddable says so explicitly with its reason. See
[data and erasure](data-and-erasure.md#declared-inverses).

Core search includes every declared projection with comparable score bands and
a supported projection renderer, both unscoped and when selected by schema.
Tags narrow those results; they are not needed to reach a spiice's projection.
A tag-filtered search requires the projection to declare its tag column.
Structured enumeration through `Engine::query` uses that same declaration
without requiring a search projection. A schema with no search surface can
declare `SearchProjectionDecl::None { why: "...", tag_column: Some("labels") }`;
the named sidecar column contains `text[]` tags. Use `tag_column: None` when
the schema has no filterable tags. The sidecar must be keyed by memory `t`.
Migration authors can obtain the canonical projection DDL and its inverses
through `proxiima::spiice::projection_artifacts(contract)`. Reuse
`BAND_EXACT`, `BAND_RESCUE`, and `BAND_SUBSTRING` from the SDK when declaring
core-comparable bands; `LEXICAL_LANGUAGE_DEPLOYMENT_DEFAULT` explicitly selects
the deployment's lexical configuration on a memory write.
Carry the output of `proxiima::spiice::search_candidate_artifacts(contract)` in
the projection's migration, alongside its owner/text and owner/tag GIN indexes.
These generated functions return only candidates authorized by memory RLS;
their declared schema sets and score bands come from the same contract.

Automatic Fact writes bind natural keys by the registered sidecar's SQL column
types. UUID-shaped text remains text; UUID columns bind UUIDs. Selection runs
in the write transaction and follows the series' current owner, including
earlier uncommitted writes in that unit of work.

For a stateful Fact sidecar, use
`proxiima::spiice::sidecar_heads_by_key::<YourPayload>` to list current series
heads without core SQL. Prefix values match the first natural-key columns in
contract order; an empty prefix selects all keys. Choose `SidecarHeadsScope::Owner`
or `Readable` for the caller's authorized owners. The page returns admission
ids, admission times and typed key values, newest first, with a cursor for the
next page. Superseded and forgotten admissions are absent; a schema-defined
tombstone is still a head. The example spiice's `note_heads` calls this helper
for its `(notebook_id, key)` series.

`proxiima::spiice::ingest_receipt` reads one Fact receipt by owner, source id
and ingest key on a transaction the caller already holds, so a shared lock
in that transaction covers the read. It share-locks the admission until the
caller finishes the transaction. It returns the admission id, series handle
and schema, or absence, and it does not admit a row. Owner row security
applies, so a forgotten admission stays hidden. `Engine::embedding_job_diagnostics`
returns one row per embedding job of the memories the caller names, at most
`MAX_EMBEDDING_DIAGNOSTIC_UNITS`. A processing job's state carries its claim
age from the database clock; pending and failed jobs do not. Coverage
counters stay on `Engine::embedding_coverage`.

Use `proxiima::spiice_bundle!` to register the payloads, contract, sidecars, and
`NamedMigrator` together. Its rustdoc shows all parameters. Use a manual
`SpiiceBundle` implementation only when composition needs it. Registry freeze
refuses inconsistent declarations; migrations and the PG catalog tests verify
the physical half. Neither check replaces the other.

Add tools under the spiice prefix. A handler implements the typed `Tool`
contract, receives `ToolCtx`, and resolves host-wired services. Declare the
actual effect once per action; owner scope, MCP hints, REST methods, and OpenAPI
consume that declaration. A tool has no raw database pool from the SDK.

## 4. Link into the host

Implement the host's `SpiiceApp` or compose tuple bundles at build time. Register
host services through `SpiiceApp::services` and configure remote stores through
`OtherStores`; see [host integration](host-integration.md). Keep the same
Proxiima git selector across all crates in the consumer graph. Cargo package
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
