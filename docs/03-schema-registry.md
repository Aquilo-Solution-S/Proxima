# 03 — Schema Registry

The schema registry is the **typing layer for Memory payloads** —
required for all three kinds. Schemas are Rust structs implementing
one of three payload traits. The build-time-only stance, the
registration mechanism, and the substrate / composite framing live
in [08 — Core and Flavors](08-core-and-flavors.md); this doc owns
the trait shapes, sidecar tables, and migration discipline.

| Trait | Required for | Storage |
|---|---|---|
| `FactPayload`        | every Fact          | sidecar; `Memory.text` is NULL |
| `AbstractionPayload` | every Abstraction   | sidecar **alongside** `Memory.text` |
| `PerspectivePayload` | every Perspective   | sidecar **alongside** `Memory.text` |
| `EdgePayload`        | edges whose `relation` is registered with one (substrate / core relations carry no payload) | sidecar keyed on `edge_id` |

A registered schema declares the typed shape of a payload. At startup,
each flavor's `register` call adds its compile-time impls to the
engine's lookup. New memories under that schema insert into the
schema's sidecar table. When a schema evolves, the flavor ships a
new payload version + a SQL migration; the old sidecar is dropped
after the migration runs. Memory identity (`id`, `event_id`,
`citation_mapping_id`, `observed_at`) is immutable across this — only
the storage shape moves.

The typing layer's distinctive contribution is making Abstraction
and Perspective claims **queryable beyond embedding similarity** —
the typed sidecar is required for every A and P, so the query
surface is uniform across all three kinds.

## Scoping: one namespace per binary

Schemas live in **the binary's namespace** — the flat union of
`SCHEMA_ID`s registered by the flavors linked into that binary
(08 §Substrate stance, §Composite discipline). A binary that
links `proxima-code` + `proxima-learning` has both flavors'
schemas in one flat namespace, keyed by `(SCHEMA_ID,
SCHEMA_VERSION)`. `SCHEMA_ID`s are **namespace-prefixed by crate
name** — `proxima-code/forgejo-commit`, `proxima-learning/lesson`
— and the prefix is auto-derived from `CARGO_PKG_NAME` by the
`proxima_flavor!` macro (08 §Schema namespacing). Authors supply
only the short id; the macro reserves the `<crate-name>/*`
namespace and rejects author-supplied `SCHEMA_ID`s that violate
it. Cross-flavor collisions are impossible within any single
Cargo registry, since registries enforce crate-name uniqueness;
multi-registry composites are a marketplace-tier concern (13).

This is the right shape for the marketplace: the linked flavor
mix is the customer's chosen "domain reality." Two customers
running different flavor mixes have different namespaces — that
is the point. User-level differences (this user is learning
Spanish, that one German history) live in *Fact payload* fields,
not in schema variants.

Per-tenant scoping **inside** a single binary is a future
enterprise extension — extending the key to
`(SCHEMA_ID, SCHEMA_VERSION, tenant_id)` with a default tenant.
v1 ships without it.

## What a schema is

A schema is a Rust struct implementing one of three payload traits.
One struct per `(schema_id, schema_version)`.

### `FactPayload`

```rust
trait FactPayload: Serialize + Deserialize + 'static {
    const SCHEMA_ID:        SchemaId;        // stable across versions
    const SCHEMA_VERSION:   SchemaVersion;   // monotonic per id
    const SPECIAL_CATEGORY: bool;            // see §Special-category declaration
    fn render(&self) -> String;              // on-demand text view
    fn sidecar_table() -> &'static str;      // SQL table name
}
```

Example:

```rust
#[derive(Serialize, Deserialize)]
struct MemophantLessonV1 {
    subject:    String,
    lesson_no:  i32,
    title:      String,
    body:       String,
}

impl FactPayload for MemophantLessonV1 {
    // Namespace prefix auto-derived from CARGO_PKG_NAME by the
    // proxima_flavor! macro; in a `proxima-memophant` crate this
    // expands to "proxima-memophant/lesson". Authors supply only
    // the short id ("lesson") via the registration macro.
    const SCHEMA_ID: SchemaId = proxima_schema_id!("lesson");
    const SCHEMA_VERSION: SchemaVersion = 1;
    const SPECIAL_CATEGORY: bool = false;
    fn render(&self) -> String {
        format!("Lesson {} — {}: {}", self.lesson_no, self.subject, self.title)
    }
    fn sidecar_table() -> &'static str { "fact_memophant_lesson_v1" }
}
```

### `AbstractionPayload` and `PerspectivePayload`

```rust
trait AbstractionPayload: Serialize + Deserialize + 'static {
    const SCHEMA_ID:        SchemaId;
    const SCHEMA_VERSION:   SchemaVersion;
    const SPECIAL_CATEGORY: bool;          // see §Special-category declaration
    fn sidecar_table() -> &'static str;    // "abstraction_<schema>_v<n>"
}

trait PerspectivePayload: Serialize + Deserialize + 'static {
    const SCHEMA_ID:        SchemaId;
    const SCHEMA_VERSION:   SchemaVersion;
    const SPECIAL_CATEGORY: bool;          // see §Special-category declaration
    fn sidecar_table() -> &'static str;    // "perspective_<schema>_v<n>"
}
```

No `render()`. The Memory's `text` (operator-authored) is the text
view; the typed payload is structured scaffolding alongside it
(see §Selective extraction).

Example:

```rust
#[derive(Serialize, Deserialize)]
struct BugFixClusterV1 {
    repo_id:        Uuid,
    fix_kind:       FixKind,           // Logic | Type | Race | Config | Test
    affected_paths: Vec<PathBuf>,
    confidence:     f32,
}

impl AbstractionPayload for BugFixClusterV1 {
    // In a `proxima-code` crate this expands to "proxima-code/bug-fix-cluster".
    const SCHEMA_ID: SchemaId = proxima_schema_id!("bug-fix-cluster");
    const SCHEMA_VERSION: SchemaVersion = 1;
    const SPECIAL_CATEGORY: bool = false;
    fn sidecar_table() -> &'static str { "abstraction_code_bug_fix_cluster_v1" }
}
```

### `EdgePayload`

```rust
trait EdgePayload: Serialize + Deserialize + 'static {
    const SCHEMA_ID:        SchemaId;
    const SCHEMA_VERSION:   SchemaVersion;
    const RELATION_CLASS:   RelationClass;   // closed enum, see 02 §Edges
    const SPECIAL_CATEGORY: bool;            // see §Special-category declaration
    fn sidecar_table() -> &'static str;      // "edge_<schema>_v<n>"
}
```

`EdgePayload` is the same shape as `FactPayload`/`AbstractionPayload`
applied to edges. The substrate's `proxima_core.edges` row carries the
relation discriminator (`relation: text` for the flavor-level name,
`relation_class: RelationClass` for the closed substrate-level traversal
contract — see [02 §Relation registry](02-memory.md#relation-registry));
the typed payload lives in a flavor-owned sidecar table keyed on
`edge_id`.

Not every edge needs a payload. Substrate / core relations
(`core/derived-from`, `core/supersedes`, `core/parent`,
`core/motivated-by`) carry zero flavor-specific state and skip the
sidecar entirely. A flavor opts in by registering a
`RelationDescriptor` whose `relation` string maps to an `EdgePayload`
impl; the engine writes the substrate edge row and the typed sidecar
in the same atomic verb (07).

`RELATION_CLASS` on the trait is a *choice among the existing
substrate enum*, not an extension point. The traversal contract
(A/P chain queries, scope filtering) is closed by design — flavors
pick the class their relation logically falls under, then use the
`relation` string for the per-flavor specifics.

Example:

```rust
#[derive(Serialize, Deserialize)]
struct EdgeCallsV1 {
    callsite_byte_range: (u32, u32),
    is_dynamic:          bool,
}

impl EdgePayload for EdgeCallsV1 {
    // In a `proxima-code` crate this expands to "proxima-code/calls".
    const SCHEMA_ID: SchemaId = proxima_schema_id!("calls");
    const SCHEMA_VERSION: SchemaVersion = 1;
    const RELATION_CLASS: RelationClass = RelationClass::Structural;
    const SPECIAL_CATEGORY: bool = false;
    fn sidecar_table() -> &'static str { "edge_code_calls_v1" }
}
```

**Edges are immutable in v1.** When a callee chunk is rewritten, the
new chunk Fact authors fresh edges; old edges remain valid for the old
chunk and are reachable via the memory layer's supersession chain. No
edge-level head-by-natural-key query path ships in v1 — promoting
stateful edges is a focused future milestone, only when a flavor
actually demands it.

### Selective extraction — design intent

Facts are tight by nature: an EventSource's payload is a contract,
and `FactPayload` mirrors it totally. Abstractions and Perspectives
are LLM-authored under personality bias; their typed payloads
capture only the **queryable scaffolding** — fields worth indexing,
joining, or filtering. Everything else (narrative, hedging,
rationale) lives in the operator-authored `Memory.text`. Typing is
required, but **what** is typed stays selective.

Two rules follow:

1. **No JSON escape hatch.** `AbstractionPayload` and
   `PerspectivePayload` impls do not carry `extra: Map<String,
   JsonValue>` or equivalent. If a field is worth structuring,
   structure it. If not, it lives in `text`.
2. **Typed-payload fields are required.** Optional fields make the
   schema signal-less for queries. A flavor that isn't sure a
   field will always be present should leave it in `text`, not
   type it as `Option<...>`.

Embeddings live in the independent vector store ([07](docs/07-storage.md)), keyed by
`(entity_kind, entity_id)`. They are not columns on any sidecar.

## Special-category declaration

Every payload schema carries `const SPECIAL_CATEGORY: bool`. The
trait declares `false` as the default, so impls that don't carry
special-category data don't need to mention it. Controllers
handling data falling under GDPR Art. 9 (special-category personal
data: health, racial/ethnic origin, political opinions, religious
or philosophical beliefs, trade-union membership, genetic data,
biometric data, sex life or sexual orientation) or analogous
heightened-protection categories under other regimes **must
explicitly override to `true`** — the substrate trusts the
declared value and applies stricter handling on its basis. The
default exists for v1 ergonomics across the existing impl base;
the deployment-controller's responsibility to override correctly
is documented as the controller's compliance obligation under
each applicable regime.

The flag is *substrate-level metadata*, not a regulatory
self-certification:

- **`true`** flips engine behavior: the schema's sidecar is
  marked in the schema registry; the storage layer can apply
  stricter access logging; `delete_owner` ([15](15-compliance.md))
  refusal-with-reason validates against `compliance.owner_policy`
  more strictly; admin protocol surfaces special-category
  distribution in subject-access exports.
- **`false`** is the default for all bare-core and most flavor
  schemas — code, lessons, build statuses, structural edges.
  Substrate operates without special-category overhead.

The flag is per-schema, not per-row. A schema either *can*
contain Art. 9-class data or *cannot*. If a schema sometimes
carries health information and sometimes does not, split it into
two schemas with different flag values rather than mixing.

Worked example — a Memophant variant carrying mood logs would
register a separate, special-category-flagged schema:

```rust
#[derive(Serialize, Deserialize)]
struct MoodEntryV1 {
    timestamp:  DateTime<Utc>,
    mood:       MoodScore,             // 1..10
    notes:      String,                // free-form, may include health detail
}

impl FactPayload for MoodEntryV1 {
    const SCHEMA_ID: SchemaId = proxima_schema_id!("mood-entry");
    const SCHEMA_VERSION: SchemaVersion = 1;
    const SPECIAL_CATEGORY: bool = true;       // Art. 9 — health data
    fn render(&self) -> String {
        format!("Mood {} at {}: {}", self.mood, self.timestamp, self.notes)
    }
    fn sidecar_table() -> &'static str { "fact_memophant_mood_entry_v1" }
}
```

A non-EU deployer who has no equivalent regulation may set
`SPECIAL_CATEGORY = false` even on schemas that would qualify
under GDPR; the substrate enforces *the declared value*, not a
regime-specific classification. The declaration is a controller
decision, not a substrate inference.

Trait families with their own homes — `GoalPayload`
([06](06-goals-and-self.md)), `CitedObjectPayload` /
`CitationMappingPayload` ([11](11-citations.md)) — carry the same
const with the same semantics.

## Sidecar tables

Sidecar tables are hand-written SQL migrations in the flavor crate:

```sql
-- migrations/2026MMDDHHMM_memophant_lesson_v1.sql
CREATE TABLE fact_memophant_lesson_v1 (
    memory_id  uuid PRIMARY KEY REFERENCES memory(id) ON DELETE CASCADE,
    subject    text NOT NULL,
    lesson_no  int NOT NULL,
    title      text NOT NULL,
    body       text NOT NULL
);
CREATE INDEX ON fact_memophant_lesson_v1 (subject);
CREATE INDEX ON fact_memophant_lesson_v1 (lesson_no);
```

Migration tracking via `schema_migrations` ([07](docs/07-storage.md)). Sidecar table name
matches `<Trait>::sidecar_table()`.

Every memory written under a schema inserts into `memory` (the shared
row) and the sidecar; both rows share `memory_id`. Exactly one sidecar
table per `(schema_id, schema_version)`.

Naming convention by kind:

- `fact_<schema>_v<n>`
- `abstraction_<schema>_v<n>`
- `perspective_<schema>_v<n>`
- `edge_<schema>_v<n>` — keyed on `edge_id` instead of `memory_id`

## Stateful Fact schemas — head-by-natural-key

Some Fact schemas track the **state of an external reality** rather
than one-shot observations: file revisions in a repo, build statuses
on a CI pipeline, sensor snapshots, the current state of an external
record. Each indexing pass or webhook tick produces a fresh Fact
under a stable **natural key** declared by the schema:

| Schema example | Natural key |
|---|---|
| `code/file-revision-v1` | `(repo_id, file_path)` |
| `code/code-chunk-v1`    | `(repo_id, file_path, chunk_index)` |
| `ci/build-status-v1`    | `(pipeline_id, ref)` |

The schema's sidecar SQL declares an index over the natural-key
columns plus `observed_at` (or `created_at`); the **head query** is
`latest by natural key, ordered by observed_at desc, limit 1` per
key. The engine's query planner expresses heads-only filters via that
index; older Facts under the same key remain readable as historical
observations.

**No `supersedes` link is involved.** The trauma test (02) holds: each
Fact is a frozen observation. "Current state" is a *computed*
projection over the observation stream, not a lineage edge — exactly
parallel to how Self is a query over `(P_active, G_active)` and not a
stored row.

**Deletions are observations, not retractions.** When the source sees
the underlying reality vanish (a file deleted, a build cancelled), it
emits a fresh Fact under the same natural key with a discriminating
`state: Tombstone` enum field on the sidecar. Same schema, different
sidecar row shape — still under the natural key, still answerable by
the same head query. Authors keep the immutability rule by *not*
introducing a separate "tombstone schema": one schema per stateful
projection, the state field carries presence-vs-absence.

Stateless Fact schemas (commit events, chat messages, page extracts)
do not need a natural key — each Fact stands alone. The pattern above
applies only when "what is the latest X" is a meaningful question for
that schema.

## Registration

See [08 §Registration mechanism](08-core-and-flavors.md#registration-mechanism). The macro form is the user-facing surface; from this doc's perspective, schemas are activated at link time and frozen there.

## How memories reference schemas

Every memory carries `(schema_id, schema_version)` on its `Memory`
row — Facts, Abstractions, and Perspectives alike. The sidecar table
is determined by `(kind, schema_id, schema_version)`. An Abstraction
under `code-bug-fix-cluster v1` lives in
`abstraction_code_bug_fix_cluster_v1`.

When a schema is migrated to a new version, memories stay in place
in the old sidecar until backfill moves them. Memory identity (id,
citation, observed_at, event_id) stays untouched — only the typed
payload's storage shape changes. For A/P, the operator-authored `text`
also stays untouched across schema migrations. The version pointer is
stored on the parent row and repeated on the wire via
`change_event.entity_schema_version`.

Cross-schema queries go through the engine's query planner, which knows
which tables to union when a query spans schemas. Within a single
schema, queries are plain SQL against the sidecar.

## Schema evolution: code + migration

Schema evolution is mediated by a payload version bump shipped
alongside an explicit migration function in the flavor crate. Same
discipline applies to all three traits. There is no "register a new
version and let old data sit forever" path — old data either
migrates forward or is dropped explicitly.

```rust
trait SchemaMigration {
    type Old: Payload;        // FactPayload | AbstractionPayload | PerspectivePayload
    type New: Payload;        // same trait family as Old

    /// Total transformation. Must succeed for every old payload, or fail
    /// the migration explicitly.
    fn migrate(&self, old: Self::Old, ctx: &MigrationCtx) -> Self::New;
}
```

`Old` and `New` must implement the same payload trait — Fact schemas
migrate to Fact schemas, Abstraction to Abstraction, Perspective to
Perspective. Cross-kind migration is forbidden (a memory's `kind`
never changes across its lifecycle; layering invariant 1).

The deploy flow for a new schema version:

1. Flavor crate adds `PayloadV{n+1}` struct + sidecar SQL migration
   + `SchemaMigration` impl.
2. Deploy runs SQL migration: creates the new sidecar table.
3. Backfill runs as a chunked, atomic, resumable stream — see
   §Streaming migration discipline. The old sidecar's residual row
   count is the live progress meter.
4. Once the old sidecar is empty, it is dropped. The new version
   becomes the active (only) version.

While the migration is running, the schema is in a `Migrating`
state. Inserts of new memories go to the new version. Reads span
both sidecars: V{n} rows are projected through `migrate` at read
time; the same fn powers backfill and read-projection.

### Streaming migration discipline

A single global `INSERT … SELECT` is unworkable once a sidecar
holds 10⁷+ rows (lock contention, undo bloat, replication lag).
Backfill runs as a sequence of bounded transactions, each
advancing one chunk from old to new atomically:

```sql
BEGIN;
  -- chunk = next N memory_ids in <old> ordered by memory_id
  INSERT INTO <new> SELECT migrate(old) FROM <old>
    WHERE memory_id IN (chunk)
    ON CONFLICT (memory_id) DO NOTHING;
  DELETE FROM <old> WHERE memory_id IN (chunk);
COMMIT;
```

Two writes, one transaction. Version is implicit in sidecar table
membership — no parent-row update needed. A crash mid-backfill
leaves a coherent split: some ids in the new sidecar, the rest still
in the old. Resuming is "run the next chunk." The old sidecar's
residual row count is the migration meter at any point in time.

The migration is fully append-only at the memories level: INSERT only
into the new sidecar, DELETE only from the old. No UPDATE on memories
anywhere.

Idempotence falls out of two properties: `migrate` is
deterministic given `MigrationCtx` (see §Properties below), and
the INSERT uses `ON CONFLICT (memory_id) DO NOTHING`. Re-running
a chunk after a partial commit failure is safe. No checkpoint
table beyond the old sidecar's residual count.

Reads during the window union V{n} rows (projected through
`migrate` at read time) with V{n+1} rows. Additive migrations —
the common case where V{n+1} only adds fields V{n} didn't have —
have **zero** read inconsistency: the projection is loss-free.
Genuinely destructive migrations (V{n+1} drops or compresses a
V{n} field) accept a brief window where the dropped field is
unavailable for un-migrated rows; the "Information-preserving by
default" rule below requires this be explicit.

Throughput is a knob: chunk size `N`, sleep between chunks, max
concurrent chunks. Bounded, never a wall.

### Properties of the migration function

- **Total.** Must produce a `New` for every `Old` payload it sees. Partial
  migrations are not supported. If the developer can't make it total
  (e.g., a NOT NULL field with no sensible default), the migration must
  reject the registration upfront.
- **Pure-ish.** May read external state via `MigrationCtx` (e.g., look up
  the citation's source to fill a new field), but should be deterministic
  given that state. The migration is replayable in case of restart.
- **Information-preserving by default.** The migration should not silently
  drop fields. Where it does, the developer should opt in explicitly —
  this is the system asking "are you sure you want to forget this?"

### Why migration over additive

Forever-additive sidecars accumulate dead schemas, complicate the planner
(every query unions more tables over time), and pretend that "old fields
are still there" when in practice no consumer reads them. Forcing a
migration function makes the developer state, explicitly, what should
happen to historical data when the shape changes.

It also keeps memory immutability where it belongs — at the
**identity and provenance** level (event_id, citation, what was
observed), not at the storage-representation level. The memory has
not changed; its typed representation has been re-shaped to match
the current schema.

## Renderer (Facts only)

`FactPayload::render(&self) -> String` is the on-demand text view
for Facts. Called whenever a consumer needs text from a Fact: F→A
prompt construction, UI display, debug summaries.

The renderer is **deterministic and cheap** by construction. No
LLM calls, no expensive transforms. Output is never stored; every
call re-renders.

There is no renderer for `AbstractionPayload` or `PerspectivePayload`.
The Memory's `text` (operator-authored) is the text view; the typed
payload is structured scaffolding alongside it, not a substitute.
This asymmetry follows from the trait-purpose split: Facts have no
narrative author, so a deterministic renderer must produce text;
A/P always have an operator-authored narrative, so a renderer would
either duplicate it or compete with it.

There is no "dream renderer" or "enriched description" mechanism.
A later pass producing richer understanding produces a *new memory*
(an Abstraction citing the Fact), not a write-over of any existing
memory. See 02.

## What this gives us

(For the architectural rationale — domain-agnostic core, type
safety end-to-end — see [08 §Why this is the right cut](08-core-and-flavors.md#why-this-is-the-right-cut).
The points below are typing-layer-specific.)

- **Typed query surface across all three layers.** Fact,
  Abstraction, and Perspective fields are real SQL columns.
  Filters, joins, aggregations work without JSON traversal — and
  without falling back to embedding similarity at the A/P layer.
- **Disciplined evolution.** Schema changes go through explicit
  `SchemaMigration` impls; old data migrates forward or is dropped
  explicitly. No silent schema-version sprawl.
- **No stored Fact rendering.** Fact text is derived on-demand by
  deterministic renderers. A/P text is operator-authored once at
  creation and never rewritten; richer text is a *new* memory.

## What this does not do

- It does not validate semantic content of payloads, only their
  typed shape.
- It does not replace `Memory.text` for A/P. The typed sidecar is
  always present alongside `text`; the sidecar carries selective
  scaffolding, the text carries the operator-authored narrative.
- It does not handle access control. v1 has one namespace per
  binary (the union of linked flavors' `SCHEMA_ID`s). Per-tenant
  scoping inside a binary is a future enterprise extension;
  authorization is a separate concern.

## Anchors

- `scoping-one-namespace-per-binary`
- `what-a-schema-is`
- `factpayload`
- `abstractionpayload-and-perspectivepayload`
- `selective-extraction-design-intent`
- `special-category-declaration`
- `sidecar-tables`
- `stateful-fact-schemas--head-by-natural-key`
- `registration`
- `how-memories-reference-schemas`
- `schema-evolution-code-migration`
- `streaming-migration-discipline`
- `properties-of-the-migration-function`
- `why-migration-over-additive`
- `renderer-facts-only`
- `what-this-gives-us`
- `what-this-does-not-do`
