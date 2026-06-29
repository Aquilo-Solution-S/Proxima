# 03 — Schema Registry

Schema registry = build-time typing layer for payload sidecars.

Owned here:

| Family | Entity row | Sidecar key | Text rule |
|---|---|---|---|
| `FactPayload` | `proxima_core.memories` | `memory_id` | no stored `text`; render on demand |
| `AbstractionPayload` | `proxima_core.memories` | `memory_id` | operator-authored `text` required |
| `PerspectivePayload` | `proxima_core.memories` | `memory_id` | operator-authored `text` required |
| `EdgePayload` | `proxima_core.edges` | `edge_id` | no text |

Same registry, separate owner docs:

| Family | Entity row | Doc |
|---|---|---|
| `GoalPayload` | `proxima_core.goals` | [06](06-goals-and-self.md#goal-entity) |
| `CitedObjectPayload` | `proxima_core.cited_objects` | [11](11-citations.md#trait-families) |
| `CitationMappingPayload` | `proxima_core.citation_mappings` | [11](11-citations.md#trait-families) |

Registry rules:

| Rule | Consequence |
|---|---|
| `(schema_id, schema_version, kind)` identifies one payload shape | no untyped Memory payload |
| sidecar-backed schemas declare a qualified sidecar table | no inferred table naming |
| every sidecar-backed write inserts the entity row and sidecar row atomically | no orphan typed storage |
| registry freezes at startup from core plus linked flavors | no runtime schema registration |
| schema evolution moves sidecar bytes only | entity identity and provenance stay fixed |
| `CitedObject` / `CitationMapping` schemas may be *opaque* — content-addressed blobs with no Rust payload type | F/A/P/Goal/Edge are never opaque |

An opaque schema is registered through `FlavorRegistry::add_opaque_schema`
and carries no validator, no CBOR encoder, and no sidecar table; its
payload is a blob addressed by content hash. `FlavorRegistry::freeze`
asserts every other schema is fully typed — a validator dropped from a
typed schema fails the build rather than silently disabling validation.

Optional typed-sidecar exceptions:

| Family | Sidecar rule |
|---|---|
| Fact | optional; required when the Fact payload has schema-owned columns |
| Abstraction | required |
| Perspective | required |
| Goal | optional |
| Edge | required only when relation descriptor declares payload schema |
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
| `schema_version` | monotonic version for that id |
| `kind` | closed `PayloadKind` |
| `sidecar_table` | optional/required per family; qualified SQL table when present, e.g. `proxima_code.commit_v1` |
| `special_category` | declared compliance flag; see §Special-category declaration |

Fact-only metadata:

| Field | Meaning |
|---|---|
| `render()` | deterministic text view |
| `natural_key_columns` | non-empty only for stateful Fact schemas |
| `tombstone` | optional state discriminator for stateful Fact deletion observations |

Edge-only metadata:

| Field | Meaning |
|---|---|
| `relation_class` | closed substrate traversal class |
| `payload_schema` on relation descriptor | opt-in sidecar payload for that relation |

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

A/P schema = typed scaffolding beside immutable operator-authored text.

Rules:

| Rule | Consequence |
|---|---|
| `Memory.text` required | narrative, rationale, hedging live there |
| sidecar row required | every A/P has a queryable payload |
| no `render()` | text is authored, not derived |
| no `extra: json` / map escape hatch | fields worth querying become real typed fields |
| selective structure | fields not worth querying stay in `text` |

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

### `EdgePayload`

Edge schema = optional typed sidecar for one registered relation.

Substrate edge row:

| Column | Meaning |
|---|---|
| `relation` | flavor-qualified relation id |
| `relation_class` | closed substrate class |
| source endpoint | Memory or Goal |
| target endpoint | Memory or Goal |
| owner pair (`owner_kind`, `owner_id`) | same owner as both endpoints |
| authorship | edge author class |

Payload rule:

| Relation descriptor | Storage |
|---|---|
| no `payload_schema` | edge row only |
| `payload_schema = E` | edge row + `E` sidecar keyed by `edge_id` |

Core substrate relations carry no payload:

| Relation | Class |
|---|---|
| `core/derived-from` | `Provenance` |
| `core/supersedes` | `Supersession` |
| `core/inspires` | `Causal` |
| `core/authored` | `Causal` |
| `core/depends-on` | `Structural` |
| `core/motivated-by` | `Structural` |

Flavor relations may carry payloads, e.g. `proxima-code/calls` with
`proxima_code.code_calls_v1`.

`relation_class` is not extensible by flavors. Flavors choose among the
closed classes and use `relation` + `EdgePayload` for domain detail.

Edges are immutable in v1. Rewrites produce new memories and new edges;
old edges remain attached to old observations.

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

Fact schemas may use JSON only for opaque external snapshots where the
Fact's source contract is itself opaque. That does not relax the A/P
escape-hatch rule.

## Special-category declaration

Every payload schema declares `special_category: bool`.

Semantics:

| Value | Meaning |
|---|---|
| `true` | schema can contain GDPR Art. 9 or analogous heightened-protection data |
| `false` | schema is treated as ordinary data by substrate compliance paths |

Scope:

| Scope | Rule |
|---|---|
| per schema | not per row |
| declared by controller/flavor author | not inferred by substrate |
| split schemas when mixed | do not mix special and non-special rows in one schema |

The flag feeds compliance behavior: audit emphasis, export metadata,
deletion policy checks, and administrative reporting (see
[13 §Compliance vocabulary](13-compliance.md#compliance-vocabulary)).

## Sidecar tables

Sidecar table contract, when a sidecar exists:

| Payload family | Primary key | Required FK |
|---|---|---|
| Fact | `memory_id` | `proxima_core.memories(memory_id)` |
| Abstraction | `memory_id` | `proxima_core.memories(memory_id)` |
| Perspective | `memory_id` | `proxima_core.memories(memory_id)` |
| Goal | `goal_id` | `proxima_core.goals(goal_id)` |
| Edge | `edge_id` | `proxima_core.edges(edge_id)` |
| CitedObject | `cited_object_id` | `proxima_core.cited_objects(cited_object_id)` |
| CitationMapping | `citation_mapping_id` | `proxima_core.citation_mappings(citation_mapping_id)` |

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
    memory_id uuid PRIMARY KEY REFERENCES proxima_core.memories(memory_id),
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
| `PresentOnly` hides tombstone heads | does not revive older present rows |

Schema metadata:

| Field | Meaning |
|---|---|
| `natural_key_columns` | sidecar columns grouped for heads-only queries |
| `tombstone.column` | sidecar discriminator column |
| `tombstone.value` | discriminator value treated as absent head |

Stateless Fact schemas leave `natural_key_columns` empty.

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
| `schema_version` | active version at write time |
| `kind` | Fact has `NULL` kind in storage variant; A/P carry derived kind |

Lookup:

```
(kind, schema_id, schema_version)
  -> SchemaInfo
  -> sidecar_table
  -> join by memory_id
```

For A/P, schema migration never rewrites `Memory.text`.

For Facts, schema migration never rewrites event identity or citation
mapping.

## Schema evolution: code + migration

Schema evolution changes payload storage representation, not entity
identity.

Allowed:

| From | To |
|---|---|
| Fact vN | Fact vN+1 |
| Abstraction vN | Abstraction vN+1 |
| Perspective vN | Perspective vN+1 |
| Edge vN | Edge vN+1 |

Forbidden:

| Change | Reason |
|---|---|
| Fact -> Abstraction | layer/kind change |
| Abstraction -> Perspective | layer/kind change |
| old version left as permanent parallel schema | planner drift |
| parent entity id replacement | breaks identity |
| silent lossy migration | unowned forgetting |

Deploy discipline:

1. Add new payload type/version.
2. Add new sidecar SQL table.
3. Add explicit migration/backfill path.
4. Write new entities to the new version.
5. Read old and new during migration window.
6. Drop old sidecar only after backfill completes.

This section is a discipline, not a license for runtime registration.

### Streaming migration discipline

Large sidecars migrate in bounded chunks.

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

### Why migration over additive

Permanent additive schemas accumulate dead tables and force every query
to union historical shapes.

Migration keeps:

| Surface | Stable |
|---|---|
| entity id | yes |
| event id / citation | yes |
| A/P text | yes |
| provenance edges | yes |
| active query shape | yes |

Only the typed sidecar representation changes.

## Renderer (Facts only)

`FactPayload::render()` is the deterministic text view for Facts.

Rules:

| Rule | Consequence |
|---|---|
| cheap | no LLM call |
| deterministic | same payload, same text |
| not stored | no Fact `text` column value |
| prompt/UI/debug only | not identity |

No renderer for A/P. Their `Memory.text` is the authored text view.

No "dream renderer." Dream/wake passes write new Abstractions,
Perspectives, Goals, or edges through flavor-declared operators (see
[02 §Wake / Dream / Write](02-memory.md#wake--dream--write)).

## What this gives us

| Capability | Source |
|---|---|
| typed query over F/A/P | sidecar tables |
| typed edge metadata | `EdgePayload` + relation descriptor |
| schema-aware UI/protocol | Schema verb |
| compliance classification | `special_category` |
| current-state projections | stateful Fact heads |
| migration without identity churn | sidecar-only evolution |

## What this does not do

| Non-goal | Owner |
|---|---|
| semantic truth validation | operators / sources |
| access control | owner scoping and protocol guards |
| runtime schema registration | forbidden by 08 |
| replacing A/P text | forbidden by 02 |
| authoritative causal chains | query only; see [02 §Causal chain query](02-memory.md#causal-chain-query) |
| embedding storage | independent vector store; see [07](07-storage.md#vector-store--independent) |

## Anchors

- `scoping-one-namespace-per-binary`
- `what-a-schema-is`
- `factpayload`
- `abstractionpayload-and-perspectivepayload`
- `edgepayload`
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
