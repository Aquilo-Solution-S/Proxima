# 03 — Schema Registry

Schema registry = build-time typing layer for payload sidecars.

Owned here:

| Family | Entity row | Sidecar key | Text rule |
|---|---|---|---|
| `FactPayload` | `proxima_core.memory` | `t` | no stored `text`; render on demand |
| `AbstractionPayload` | `proxima_core.memory` | `t` | typed Content required; authoring text feeds embedding/search, not Memory |
| `PerspectivePayload` | `proxima_core.memory` | `t` | typed Content required; authoring text feeds embedding/search, not Memory |

There is no edge payload family and no edge table. Pins are
`memory.origins` / `memory.refs` (see [16](16-edges.md)); a payload's
`references()` declaration is what writes `refs`.

Same registry, separate owner docs:

| Family | Entity row | Doc |
|---|---|---|
| `GoalPayload` | `proxima_core.goal` | [06](06-goals-and-self.md#goal-entity) |
| `CitedObjectPayload` | `proxima_core.blob` | [11](11-citations.md#trait-families) |
| `CitationMappingPayload` | optional sidecar on `memory.blob_id` | [11](11-citations.md#trait-families) |

Registry rules:

| Rule | Consequence |
|---|---|
| `(schema_id, schema_version, kind)` identifies one payload shape | no untyped payload |
| sidecar-backed schemas declare a qualified sidecar table | no inferred table naming |
| every sidecar-backed write inserts the entity row and sidecar row atomically | no orphan typed storage |
| registry freezes at startup from core plus linked flavors | no runtime schema registration |
| schema evolution moves sidecar bytes only | entity identity and provenance stay fixed |
| `CitedObject` / `CitationMapping` schemas may be *opaque* — content-addressed blobs with no Rust payload type | F/A/P/Goal are never opaque |

An opaque citation schema is registered through
`FlavorRegistry::try_add_opaque_schema` and carries no protocol ingress
parser, JSON schema, or sidecar table. Its payload enters only through the
explicit citation APIs; protocol payload ingress rejects it.
`FlavorRegistry::try_freeze` defensively rejects an opaque F/A/P/Goal
descriptor, and `FlavorRegistryFrozen` has no public constructor or mutation
surface outside successful freeze.

Optional typed-sidecar exceptions:

| Family | Sidecar rule |
|---|---|
| Fact | optional; required when the Fact payload has schema-owned columns |
| Abstraction | required |
| Perspective | required |
| Goal | optional |
| CitedObject | required for typed cited-object schemas |
| CitationMapping | optional; pure links need no sidecar |
| Opaque citation schemas | none |

Typed sidecars are what make A/P queryable beyond embeddings. Vector
similarity is a query aid, not the schema surface (see
[07 §Vector store](07-storage.md#vector-store--independent)).

## Scoping: one namespace per binary

Namespace = flat union of core schemas plus schemas registered by the
flavors linked into the binary.

Schema ids are qualified by their owning registry namespace:

| Flavor crate | Short id | Registered schema id |
|---|---|---|
| `proxima-code` | `commit-v1` | `proxima-code/commit-v1` |
| `proxima-code` | `commit-summary-v1` | `proxima-code/commit-summary-v1` |
| `core` | `agent-derivation-v1` | `core/agent-derivation-v1` |

`core/` is reserved for substrate schemas. `proxima_flavor!` owns flavor
prefix discipline (see [08 §Macro Surface](08-core-and-flavors.md#macro-surface)).
Authors provide the short id; the namespace prevents collisions inside
one composite binary.

User/domain variation belongs in payload fields, not schema-id forks.

No v1 tenant dimension. A future enterprise key would extend the lookup
to `(tenant_id, schema_id, schema_version, kind)`.

## What a schema is

Schema = one concrete payload type registered as one payload family.

Required registry metadata:

| Field | Meaning |
|---|---|
| `schema_id` | stable qualified id |
| `schema_version` | declared payload version; for Fact/Abstraction/Perspective it is the unique frozen version of `(kind, schema_id)`, so a new logical shape also needs a new `schema_id` |
| `kind` | closed `PayloadKind` |
| `sidecar_table` | optional/required per family; qualified SQL table when present, e.g. `proxima_code.commit_v1` |

Fact-only metadata:

| Field | Meaning |
|---|---|
| `render()` | deterministic text view |
| `natural_key_columns` | non-empty only for stateful Fact schemas |
| `tombstone` | optional state discriminator for stateful Fact deletion observations |

Connection metadata (every family):

| Field | Meaning |
|---|---|
| `references()` | the node references this payload's fields carry; ingest derives one `Reference` index entry per declaration, in the node write's own transaction. Default: none. |

`references()` is the *only* way a schema writes `memory.refs`.
There is no edge kind to choose and no relation to register. Every
address is a pin (`ReferenceBinding::Pin`).

### `FactPayload`

Fact schema = total typed representation of one observation.

Rules:

| Rule | Consequence |
|---|---|
| Fact row has no stored `text` | UI/prompts call `render()` |
| sidecar row optional | required for schema-owned Fact columns; absent for citation-bodied Facts |
| identity is not payload hash | `memory_id` remains UUIDv7 (see [07 §ID types](07-storage.md#id-types)) |
| Fact has no `supersedes` | state is a query over observations |

Nullable fields are allowed only when the source contract itself is
nullable. They are still typed schema fields, not an escape hatch.

### `AbstractionPayload` and `PerspectivePayload`

A/P schema = durable typed Content. The authoring request's text is an
embedding/search input, not a Memory column.

Rules:

| Rule | Consequence |
|---|---|
| no `Memory.text` | durable narrative/rationale fields belong to typed Content |
| sidecar row required | every A/P has a queryable payload |
| no `render()` | search/body projections come from typed sidecar fields |
| no `extra: json` / map escape hatch | fields worth querying become real typed fields |
| explicit structure | durable fields remain schema-owned and typed |

Nullable A/P fields are allowed when null is part of the domain model
(`repo_id = NULL` for global code perspective, optional idempotency key,
optional review location). Nullability must not be used to avoid
declaring distinct schemas.

Cross-domain Fact synthesis rule:

```
F(D1) + F(D2) -> A(D1,D2)
```

No direct semantic Fact-to-Fact edge is needed. The typed Abstraction
is the cross-domain join object; a Perspective may frame or reuse it.

### Reference fields

A schema does not register a connection vocabulary. It declares which of its
own fields point at other nodes, and ingest turns each declaration into one
`reference` row:

```rust
fn references(&self) -> Vec<PayloadReference> {
    vec![PayloadReference::memory(
        "work_item_memory_id",
        EntityKind::Fact,
        self.work_item_memory_id,
    )]
}
```

The substrate edge row that results carries only source, target, kind, owner
and `created_at` — no relation, no class, no payload, no authorship column,
no id. Domain detail stays in the payload that declared the reference: ten
call sites from chunk A to chunk B are **one** index row and ten entries in
A's payload.

Constructors and what they bind to:

| Constructor | Address | Binding |
|---|---|---|
| `PayloadReference::memory` | a `memory` row (`t`) | pins that observation |
| `PayloadReference::goal` | a `goal` row (`t`) | pins that Goal |

Rules:

| Rule | Consequence |
|---|---|
| the declaration is the only source of `reference` rows | the edge set is a function of node content, and re-deriving it from payloads reproduces it exactly |
| the primary key is `(source, target, kind)` | replaying a write re-asserts the row; no duplicate, no id to reconcile |
| kind is never a parameter | a schema cannot invent a third kind; a feature that seems to need one is missing a node |

Edges are immutable. Rewrites produce new memories and new index rows; the
rows attached to old observations stay attached to them.

### Selective extraction — design intent

Facts mirror source payloads. A/P payloads expose only the fields useful
for filters, joins, aggregations, dispatch, or compliance.

Allowed:

| Shape | Use |
|---|---|
| scalar columns | filtering / ordering |
| SQL enum columns | closed domain vocabulary |
| typed nested structs serialized into declared columns | compact structured Fact detail |
| nullable columns | real domain nullability |

Forbidden:

| Shape | Reason |
|---|---|
| `extra json/jsonb` on A/P | unbounded schema drift |
| semantic data only in embeddings | not queryable or auditable |
| sidecar-less A/P | violates typed A/P invariant |
| replacing A/P `text` with payload fields | loses authored narrative |

Typed Fact schemas may use JSON-valued fields for external snapshots whose
source contract is itself opaque. The Fact schema and ingress remain typed;
this does not relax the A/P escape-hatch rule.

## Special-category declaration

There isn't one. `SchemaContract::special_category` was a per-schema `bool`
every schema declared and nothing read: no verb branched on it, no erase or
export leg consulted it, and the kernel deliberately does not reason over
special-category at all (`docs/lean/COVERAGE.md`, SR-30..33, D16). It is
deleted, not demoted to a marker — see
[13 §Declared metadata](13-compliance.md#declared-metadata) for what a host
with Art. 9 obligations does instead.

The scoping rule it carried survives it, because it was never about the flag:
a schema is per-schema, not per-row, so rows needing different handling belong
in a different schema — one with its own surfaces, `EraseRule` and
`ExportRule`, all three of which are read.

## Sidecar tables

Sidecar table contract, when a sidecar exists:

| Payload family | Primary key | Required FK |
|---|---|---|
| Fact | `t` | `proxima_core.memory(t)` |
| Abstraction | `t` | `proxima_core.memory(t)` |
| Perspective | `t` | `proxima_core.memory(t)` |
| Goal | `t` | `proxima_core.goal(t)` |
| CitedObject | `blob_id` | `proxima_core.blob(blob_id)` |
| CitationMapping | optional sidecar | `memory.blob_id` is the link |

Rules:

| Rule | Consequence |
|---|---|
| sidecar table is core- or flavor-owned SQL | owning crate owns migrations |
| table name must be schema-qualified | `proxima_code.commit_v1`, not `commit_v1` |
| table name must equal registry metadata | query planner joins declared tables |
| one sidecar table per `(kind, schema_id, schema_version)` | no mixed-version table |
| columns use SQL types / SQL enums for closed vocabularies | no fake enum strings |
| `*_saturating` macro casts are for lossy compatibility only | validate value-bearing widths or use exact-width casts |

Example shape:

```sql
CREATE TABLE proxima_code.commit_v1 (
    t uuid PRIMARY KEY REFERENCES proxima_core.memory(t),
    repo_id uuid NOT NULL,
    sha text NOT NULL,
    message text NOT NULL
);
```

Migration tracking belongs to storage (see
[07 §Append-only](07-storage.md#append-only)).

## Stateful Fact schemas — head-by-natural-key

Stateful Fact schema = append-only observations with a declared natural
key and a “current head” projection.

Examples:

| Schema | Natural key |
|---|---|
| `proxima-code/file-revision-v1` | `(repo_id, file_path)` |
| `proxima-code/code-chunk-v1` | `(repo_id, file_path, chunk_index)` |
| `ci/build-status-v1` | `(pipeline_id, ref)` |

Rules:

| Rule | Consequence |
|---|---|
| each observation is a new Fact | no Fact update |
| no Fact `supersedes` | current state is not lineage |
| head query orders by memory creation/observation time | latest row per natural key |
| tombstone is a Fact under the same schema/key | deletion is observed state |
| Query returns the tombstone head | deletion state is not a core filtering axis |

Schema metadata:

| Field | Meaning |
|---|---|
| `natural_key_columns` | sidecar columns grouped for heads-only queries |
| `tombstone.column` | cataloged sidecar discriminator column |
| `tombstone.value` | value identifying a deletion-observation head |

Stateless Fact schemas leave `natural_key_columns` empty.

## Declared lifecycle scope

A flavor may own a lifecycle the substrate cannot infer — a repository, a
book, a project, a revision. Its rows carry a bare `<scope>_id uuid` with no
foreign key into the flavor's registry, so no core fence separates an erase of
one scope from a concurrent write into it. A schema says which lifecycle its
rows belong to; the substrate does the fencing.

The declaration has two halves, and freeze checks them against each other.

| Half | Where | What it says |
|---|---|---|
| `const SCOPE_KIND: Option<ScopeKind>` | `FactPayload` / `AbstractionPayload` / `PerspectivePayload` | which lifecycle this schema's rows belong to (default `None`) |
| `fn scope_id(&self) -> Option<Uuid>` | the same payload impl | which row of it this value belongs to |
| `ScopeDecl` in `FlavorContract::scopes` | the flavor contract, once per kind | the scope registry's schema-qualified table, id column, owner kind column and owner id column |

`ScopeKind` is a `&'static str` newtype, namespaced by convention
(`<flavor>-<scope>`, e.g. `code-repo`): the closed vocabulary lives in the
linked flavors, not in core.

Storage GENERATES from the declaration and spells no name of its own — the
fence key (`proxima-scope-fence:<scope_kind>:<owner_kind>:<owner_id>:<scope_id>`)
and the liveness probe
(`SELECT EXISTS(SELECT 1 FROM <registry_table> WHERE <owner_kind_column> = $1
AND <owner_id_column> = $2 AND <id_column> = $3)`). A renamed column is a
declaration edit, not a second place to keep in sync.

Freeze refusals, in the shape of the sidecar-declaration guards:

| Shape | Refusal |
|---|---|
| a payload names a `ScopeKind` no linked contract declares | `ScopeNotDeclared` |
| two contracts declare one `ScopeKind` | `DuplicateScopeDeclaration` |
| a declaration's registry table is not schema-qualified, or a column name is empty | `InvalidScopeDeclaration` |

There is no runtime registration path, and no generic scope erase: what "one
scope's rows" means is the flavor's knowledge. What the substrate guarantees
is the other half — that no admission of a scoped payload, from any caller,
can slip past the fence that erase holds. See
[07 §Lifecycle Lock Ordering](07-storage.md#lifecycle-lock-ordering) for the
lock order and
[09 §Declare the scope](09-developing-flavors.md#declare-the-scope-the-substrate-fences-every-admission-your-erase-takes-it-exclusive)
for the flavor author's side.

Payloads whose scope column is nullable return `None` for the rows that name
no scope, and those rows are unscoped in fact as well as in declaration.

## Registration

Registration is build-time only (see
[08 §Freeze Guards](08-core-and-flavors.md#freeze-guards)).

Flow:

```
linked flavor crates
  -> proxima_flavor! registration
  -> FlavorRegistry
  -> freeze at startup
  -> Schema verb exposes immutable registry
```

No runtime registration endpoint. No `Registrant::Runtime`.

## How memories reference schemas

Memory row stores:

| Column | Rule |
|---|---|
| `schema_id` | flavor-qualified id |
| `kind` | closed Memory layer; the stored selector is `(kind, schema_id)` |
| `sidecar_tables` | stamped sidecar surfaces for this row; an integrity check, never a version selector |

Lookup:

```
(kind, schema_id)
  -> unique registered Memory schema declaration
  -> sidecar_table
  -> join by memory_id
```

`proxima_core.memory` and `memory_head` do not store `schema_version`.
Every visible F/A/P row must resolve exactly one registered declaration for
its stored selector. A missing or ambiguous declaration fails closed; readers
never invent version 1 or omit an authorized row. A sidecar-less registration
is still a complete declaration and truthfully hydrates its version with no
payload/text.

For A/P, no `Memory.text` exists to migrate. A changed logical payload shape
needs a new schema id and new admissions; a storage-only backfill may preserve
an admitted `t` only when the logical schema contract is unchanged.

For Facts, schema migration never rewrites Fact `MemoryId`, optional receipt
metadata, or citation mapping.

## Schema evolution: code + migration

Schema evolution changes payload storage representation, not entity
identity.

Allowed:

| From | To |
|---|---|
| Fact `(F, schema-a)` vN | Fact `(F, schema-b)` vN+1 |
| Abstraction `(A, schema-a)` vN | Abstraction `(A, schema-b)` vN+1 |
| Perspective `(P, schema-a)` vN | Perspective `(P, schema-b)` vN+1 |

Each transition above changes the stored selector. Reusing `schema-a` for both
versions is forbidden because a Memory row cannot say which version it holds.

Forbidden:

| Change | Reason |
|---|---|
| Fact -> Abstraction | layer/kind change |
| Abstraction -> Perspective | layer/kind change |
| two F/A/P versions under one `(kind, schema_id)` selector | Memory stores no version, so reads would be ambiguous |
| parent entity id replacement | breaks identity |
| silent lossy migration | unowned forgetting |

Deploy discipline:

1. Add a new payload type/version and a new stored selector for F/A/P.
2. Add a new sidecar SQL table when required.
3. Add an explicit migration/backfill path if existing `t` values must move.
4. Write new entities to the new selector.
5. Retain the old selector only while its explicit migration/read contract is supported.
6. Drop old sidecar only after backfill completes.

Goal and citation registrations retain their existing
`(schema_id, schema_version, kind)` duplicate law; this Memory selector rule
does not change them.

This section is a discipline, not a license for runtime registration.

### Streaming storage-backfill discipline

Large auxiliary/projection tables and storage-only sidecar layouts migrate in
bounded chunks. This is not a dual-version F/A/P schema window: the logical
payload selector and shape must remain unchanged. A new F/A/P shape follows
the new-selector/new-admission rule above.

Chunk invariant:

```
BEGIN;
  INSERT new_sidecar(memory_id, ...)
    SELECT memory_id, migrate(old.*)
    FROM old_sidecar
    WHERE memory_id IN (chunk)
    ON CONFLICT (memory_id) DO NOTHING;

  DELETE FROM old_sidecar
    WHERE memory_id IN (chunk);
COMMIT;
```

Properties:

| Property | Rule |
|---|---|
| bounded transaction | no global lock wall |
| idempotent insert | safe retry |
| residual old rows | progress meter |
| split old/new sidecars | coherent crash state |
| no entity table update | identity untouched |

### Properties of the migration function

| Property | Rule |
|---|---|
| total | every old payload maps or migration fails explicitly |
| deterministic | retry produces same new payload |
| same family | payload kind does not change |
| information-preserving by default | lossy migration must be explicit |
| owner-preserving | no cross-owner data movement |

### Why a storage backfill over permanent query unions

For an unchanged logical schema, permanent duplicate storage layouts
accumulate dead tables and force every query to union historical layouts.

Migration keeps:

| Surface | Stable |
|---|---|
| entity id | yes |
| event id / citation | yes |
| admitted Memory `t` and pins | yes |
| `origin` provenance entries | yes |
| active query shape | yes |

Only the physical storage layout changes; the registered payload contract does
not.

## Renderer (Facts only)

`FactPayload::render()` is the deterministic text view for Facts.

Rules:

| Rule | Consequence |
|---|---|
| cheap | no LLM call |
| deterministic | same payload, same text |
| not stored | no Fact `text` column value |
| prompt/UI/debug only | not identity |

No renderer for A/P. Durable body/search fields come from typed sidecars;
authoring text feeds embedding/search projections without becoming a Memory
column.

No "dream renderer." Dream/wake passes write new Abstractions,
Perspectives, or Goals through flavor-declared operators; the index entries
follow from what those nodes declare (see
[02 §Wake / Dream / Write](02-memory.md#wake--dream--write)).

## What this gives us

| Capability | Source |
|---|---|
| typed query over F/A/P | sidecar tables |
| connections without a connection vocabulary | payload `references()` |
| schema-aware UI/protocol | Schema verb |
| current-state projections | stateful Fact heads |
| storage-layout migration without identity churn | unchanged logical schema plus an explicit backfill |

## What this does not do

| Non-goal | Owner |
|---|---|
| semantic truth validation | operators / sources |
| access control | owner scoping and protocol guards |
| runtime schema registration | forbidden by 08 |
| untyped A/P body escape hatch | forbidden by 02 / this registry contract |
| authoritative causal chains | query only; see [02 §Causal chain query](02-memory.md#causal-chain-query) |
| embedding storage | independent vector store; see [07](07-storage.md#vector-store--independent) |

## Anchors

- `scoping-one-namespace-per-binary`
- `what-a-schema-is`
- `factpayload`
- `abstractionpayload-and-perspectivepayload`
- `reference-fields`
- `selective-extraction-design-intent`
- `special-category-declaration`
- `sidecar-tables`
- `stateful-fact-schemas--head-by-natural-key`
- `registration`
- `how-memories-reference-schemas`
- `schema-evolution-code--migration`
- `streaming-migration-discipline`
- `properties-of-the-migration-function`
- `why-migration-over-additive`
- `renderer-facts-only`
- `what-this-gives-us`
- `what-this-does-not-do`
