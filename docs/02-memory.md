# 02 — Memory

Memory is the cognitive graph above Event Sources.

```
Reality ──EventSource──► Fact ──F→A──► Abstraction ──A→P──► Perspective
                                            └────A→Goal────► Goal
```

## Ontology at a Glance

Five terms recur throughout Proxima. Four are **node kinds**; the fifth
(Citation) is provenance, not a node. Where each lives in the schema is the
part the table names hide:

| Term | What it is | Schema home | Produced by |
|---|---|---|---|
| **Fact** | An accepted observation from an Event Source — the event stream. Never revised. | `memories` (`kind` NULL, `event_id` set) | EventSource ingest |
| **Abstraction** | A re-derivable interpretation over Facts. | `memories` (`kind = 'Abstraction'`) | `F→A` operator |
| **Perspective** | A re-derivable integration over Abstractions; the lens reads are taken through. The self-perspective anchors a personality. | `memories` (`kind = 'Perspective'`) | `A→P` operator |
| **Goal** | A desired end-state, with a lifecycle (`state`) and a parent DAG (`goal_parents`). | `goals` (its own table) | user / external / `A→Goal` operator |
| **Citation** | *Not a node.* An immutable, content-addressed outside-proof attached to a Fact ("I assert this because of that source"). | `cited_objects` + a `cited_<schema>` byte sidecar, linked to a Fact by `citation_mappings` | attached at Fact ingest |

So `memories` is **three** node kinds in one table (Fact, Abstraction,
Perspective), discriminated by `kind`; Goals are a separate axis; Citations
are bibliography hanging off Facts, never a node of their own. The same
breakdown is mirrored as `COMMENT ON` text on the tables in
`0001_init.sql`. Goal detail is in [06](06-goals-and-self.md); Citation
mechanics in [11](11-citations.md).

## The Layering Principle

Sets:

- **F** = `{ m ∈ Memory | kind = Fact }`
- **A** = `{ m ∈ Memory | kind = Abstraction }`
- **P** = `{ m ∈ Memory | kind = Perspective }`

Layer function:

```
ℓ(F)=0
ℓ(A)=1
ℓ(P)=2
```

Production rules:

| Operator | Signature | Rule |
|---|---|---|
| F→A | `2^F × Π → A` | Facts become a typed Abstraction. Cross-domain input is legal only when the output schema is an explicit cross-domain Abstraction. |
| A→P | `2^A × Π → P` | Abstractions become a typed Perspective under the active personality instance. |
| frame | `P × A_cross → Edge` | Perspective may frame a cross-domain Abstraction. Facts stay unchanged. |
| A→Goal | `2^A × Π → Goal` | Core derives / updates Goals from visible evidence (see 06). |

`Π` = active personality instance. Runtime identity is
`personality_instance_id`; type-level behavior comes from the registered
flavor. Load-bearing type evolution is a new flavor/type id; load-bearing
runtime lineage evolution is a new instance.

Forbidden:

- A→F, P→A, P→F writes.
- Upward F/A/P edges: Fact→Abstraction, Fact→Perspective,
  Abstraction→Perspective.
- Semantic or causal Fact→Fact edges.
- Mutation of existing memories by later passes.

## Why This Layering — The Trauma Test

Rules:

- Facts are Reality observations. They are accepted, not revised.
- Abstractions are re-derivable interpretations over Facts.
- Perspectives are re-derivable integrations over Abstractions.
- Personality changes affect future derivations, not existing Facts.

## The Core Entity

All memories share one identity shape:

| Field | Rule |
|---|---|
| `memory_id` | UUIDv7 for Fact, Abstraction, Perspective alike |
| `owner` | Per-row Owner from 01 |
| `schema_id`, `schema_version` | Present for every memory |
| `created_at` | Insert time |

Kind-specific content:

| Kind | Content | Citation | Text | Supersession |
|---|---|---|---|---|
| Fact | Typed `FactPayload` sidecar | Optional `citation_mapping_id` | none; render on demand | never |
| Abstraction | Typed `AbstractionPayload` sidecar + immutable `text` | none | operator-authored | allowed |
| Perspective | Typed `PerspectivePayload` sidecar + immutable `text` | none | operator-authored | allowed |

Facts are observations from Event Sources. Fact identity is the UUIDv7
`memory_id`, not the content hash; `event_id` is the EventSource
idempotency key.

Abstractions and Perspectives are derived memories. Their provenance is
edge-based, not JSON inside the memory row. Their reproducibility metadata
lives inline on the memory row: operator kind, model id, prompt version,
personality instance, wake depth.

There is no `description` field. Facts render from payload. A/P text is
the authored cognitive surface. Typed sidecars are the query surface.

## Provenance

Provenance points downward:

```
Perspective ──core/derived-from──► Abstraction ──core/derived-from──► Fact
```

Rules:

- F→A writes `Abstraction → Fact*` provenance edges.
- A→P writes `Perspective → Abstraction*` provenance edges.
- Cross-domain Fact synthesis is `Abstraction_cross → Fact*`.
- Perspective framing of cross-domain synthesis is `Perspective → Abstraction_cross`.
- Bibliographic provenance for A/P is the transitive closure to Facts and
  their citation mappings.
- Edges have no citation id. Edge authorship is reasoning provenance, not
  bibliography.

## Edges

Edges connect Memories and Goals. Memories obey F/A/P layering; Goals are a
separate entity axis (see 06).

## Relation Registry

Every edge relation resolves to a build-time `RelationDescriptor`
registered by core or a flavor (see 08). Unregistered relations are
invalid.

Closed relation classes:

| Class | Use |
|---|---|
| `Structural` | Payload / system structure. |
| `Provenance` | Derived-from lineage. |
| `Supersession` | New entity supersedes prior entity. |
| `Causal` | Perspective-relative cause / motivation. |
| `Interpretive` | Perspective-relative non-causal interpretation. |

Core relations:

| Relation | Class | Shape |
|---|---|---|
| `core/derived-from` | `Provenance` | Derived entity → evidence |
| `core/supersedes` | `Supersession` | A→A, P→P, Goal→Goal |
| `core/inspires` | `Causal` | Goal → Root Perspective |
| `core/authored` | `Causal` | Root Perspective → emitted memory |
| `core/motivated-by` | `Structural` | Goal → Fact / Abstraction evidence |

Relation classes are substrate vocabulary. Flavors add relation ids, not new
classes.

## The Directionality Rule

Universal edge constraints:

- Endpoint ids must exist.
- Declared endpoint kind must equal stored endpoint kind.
- `source.owner == target.owner == edge.owner`.
- F/A/P layer rule: `ℓ(source) ≥ ℓ(target)`.
- Goal endpoints sit outside F/A/P layer comparison; descriptor masks govern.
- Descriptor masks may tighten legal shapes, never relax F/A/P layering.
- Direct `Causal` / `Interpretive` Fact→Fact edges are forbidden.
- `Supersession` never touches Facts and requires same endpoint kind.

F/A/P matrix:

| From → To | Legal | Classes |
|---|---:|---|
| Fact → Fact | yes | `Structural`, `Provenance`; never `Causal`, `Interpretive`, `Supersession` |
| Abstraction → Fact | yes | `Provenance`, `Structural` |
| Abstraction → Abstraction | yes | `Structural`, `Supersession` |
| Perspective → Fact | yes | `Causal`, `Interpretive`, `Structural` |
| Perspective → Abstraction | yes | `Provenance`, `Causal`, `Interpretive`, `Structural` |
| Perspective → Perspective | yes | `Structural`, `Supersession`, `Causal`, `Interpretive` |
| Fact → Abstraction | no | — |
| Fact → Perspective | no | — |
| Abstraction → Perspective | no | — |

## Edge Scope Invariant

All edges are single-Owner:

```
source.owner == target.owner == edge.owner
```

Cross-owner sharing is a query/access concern, not an edge write.

Edge authorship vocabulary:

| Authorship | Use |
|---|---|
| `EventSource` | Payload-derived structural edges. |
| `OperatorFtoA` | F→A provenance. |
| `OperatorAtoP` | A→P provenance. |
| `OperatorAtoGoal` | A→Goal provenance. |
| `PerspectiveLink` | P-authored causal / interpretive framing. |
| `Engine` | Substrate-authored edges such as supersession / authored. |
| `User` | Explicit user/API-authored graph edits. |
| `ExternalAgent` | Agent-authored MCP / imported edges. |

## Causal Chain Query

Facts alone do not answer "why"; they only support correlation and
structure. Causal claims are Perspective-relative.

```
chain(f, P_active)
  = Structural Fact backbone
  + Causal / Interpretive edges authored by P_active
  + provenance closure from contributing P/A nodes to Facts
```

Rules:

- `chain(f, P_active)` is a query, not an entity.
- Different active Perspectives can produce different valid chains.
- Supersession changes which P/A heads participate in future queries; old
  chains remain reconstructable from the append-only graph.
- A materialized chain view is a cache only, never authoritative.

## Wake / Dream / Write

Dreaming is flavor-declared consolidation through ordinary wake/write paths.
No Dream entity, Dream relation class, or Core dream pipeline.

```
change_event
  -> wake entry match
  -> personality / tool decision
  -> typed Memory / Goal / Edge writes
  -> registry + edge invariant enforcement
```

Wake entries live in `personality_wake_entries`.

Dream forms:

| Form | Signature | Output |
|---|---|---|
| Compaction | `2^F × Π → A` | Abstraction |
| Reflection | `2^A × Π → P` | Perspective |
| Cross-domain synthesis | `2^F_cross × Π → A_cross` | Abstraction |
| Self/Perspective revision | `2^A × P_active × G_active → P_new` | Perspective |
| Goal reorientation | `P/A evidence → Goal write` | Goal write / supersession |

Dream outputs are ordinary writes. They obey schema registration, relation
registration, owner scope, layer direction, citation rules, and append-only
rules.

## Re-derivation and Supersession

Facts never supersede and are never superseded.

A, P, and Goals may supersede:

```
new_entity --core/supersedes--> old_entity
```

Rules:

- Supersession is append-only: new row + edge / lineage pointer.
- Endpoint kind must match.
- Facts have no `Supersession` relation.
- Stateful Fact projections use head-by-natural-key queries on sidecars
  (see 03), not supersession.
- Deletion observations are Facts with state in their sidecar, not erased
  rows.
- Hard delete exists only as compliance erasure (see 13), outside cognitive
  graph semantics.

Default lineage scope is the personality instance that authored the derived
memory. Cross-personality supersession is an explicit user/API editorial
gesture, never an operator decision.

## Assertion Lifecycle Pattern

Assertion = typed Abstraction whose sidecar carries a flavor-owned stable
key plus claim fields. Core owns lifecycle mechanics only.

```
Fact evidence* --core/derived-from--> Assertion(A)
Assertion(A_new) --core/supersedes--> Assertion(A_old)
Assertion(A) --flavor/structural-endpoint--> Fact entity head*
```

Core requirements:

- assertion payload is an `AbstractionPayload` sidecar; no generic
  relation entity;
- evidence is `core/derived-from` to Facts; citations stay Fact-only;
- endpoint refs use ordinary registered structural edges, preferably
  `FollowHead` Fact-entity endpoints for stateful entities;
- supersession writes both `memories.supersedes` and `core/supersedes` in
  the same transaction;
- current / superseded state is query-derived from heads, disposition, and
  flavor-owned validity fields.

Flavor responsibilities:

- stable assertion key shape;
- endpoint vocabulary and payload fields;
- validity scope (`Date` interval, repo commit range, etc.);
- confidence / disposition enums;
- domain MCP wrappers and projection caches.

Do not add edge citation/status fields, runtime relation vocabularies, a
core `RelationAssertion` entity, or authoritative materialized relation
edges for this pattern.

## Personality

Personality is a flavor-declared decider type plus runtime instances.

Substrate responsibilities:

- store personality instances and Root Perspective pointers;
- store wake entries;
- dispatch eligible wake entries from `change_event`;
- pass read/write authorization and tool palettes;
- record produced A/P rows with personality instance and wake depth;
- enforce registry and edge invariants.

Flavor responsibilities:

- prompt / instructions;
- self schema and default self payload;
- writeable schemas and relations;
- tool palette;
- model tier / capability requirements;
- wake entry defaults;
- decision logic.

Multiple personality instances may be active for one Owner. Same Facts or
Abstractions under different instances produce parallel lineages.

## Read-scope Matrix

Cross-personality retrieval is governed by a per-Owner boolean adjacency
matrix over personality instances:

```
M[self][other] = 1  => self may read other's A/P/Goals
M[self][other] = 0  => self excludes other's A/P/Goals
```

Rules:

- Identity diagonal: `M[p][p] = 1`.
- Facts are below the matrix: every personality sees every Fact in the Owner.
- A/P/Goals are gated by the matrix.
- Matrix asymmetry is valid.
- Matrix controls direct retrieval only. Transitive influence is represented
  by authored memories and provenance edges.
- Changing the matrix affects future reads only. Existing memories remain.
- Load-bearing read-scope evolution that needs separate lineage uses a new
  personality instance.

## What's Settled

- Strict F/A/P layering.
- Facts immutable; A/P/Goals append and may supersede.
- Cross-domain Fact synthesis is a typed Abstraction, not Fact→Fact semantics.
- A/P are always typed and always carry immutable text.
- Citations are Fact-only and bibliographic.
- Relations are build-time registered and classed by closed substrate enum.
- Edge invariants are storage-enforced (see 07).
- Causal chains and Self are queries, not entities (see 06).
- Dreaming is ordinary wake/write behavior, not a substrate component.

## Anchors

- `ontology-at-a-glance`
- `the-layering-principle`
- `why-this-layering-the-trauma-test`
- `the-core-entity`
- `provenance`
- `edges`
- `relation-registry`
- `the-directionality-rule`
- `edge-scope-invariant`
- `causal-chain-query`
- `wake-dream-write`
- `re-derivation-and-supersession`
- `personality`
- `read-scope-matrix`
- `whats-settled`
