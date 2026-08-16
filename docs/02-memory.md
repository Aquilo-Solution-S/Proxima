# 02 — Memory

Memory is the cognitive graph above source-ingested Facts.

```
Reality ──FactIngest──► Fact ──F→A──► Abstraction ──A→P──► Perspective
                                            └────A→Goal────► Goal
```

## Ontology at a Glance

Five terms recur throughout Proxima. Four are **node kinds**; the fifth
(Citation) is provenance, not a node. Where each lives in the schema is the
part the table names hide:

| Term | What it is | Schema home | Produced by |
|---|---|---|---|
| **Fact** | An accepted observation. Never revised. Receipt metadata may link it to a source identity, but receiptless Facts are valid. | `memories` (`kind` NULL, optional `receipt_id`) + optional `fact_receipts` | Fact write / receipt-backed FactIngest |
| **Abstraction** | A re-derivable interpretation over Facts. | `memories` (`kind = 'Abstraction'`) | `F→A` operator |
| **Perspective** | A re-derivable integration over Abstractions; the lens reads are taken through. Self is a query over Perspective rows and active Goal heads, not a row or authz carrier. | `memories` (`kind = 'Perspective'`) | `A→P` operator |
| **Goal** | A desired end-state with a lifecycle (`state`). Goal↔Goal topology is declared on the Goal row (`dependency_goal_ids`); the edge index is derived from it. | `goals` (its own table) | user / external / `A→Goal` operator |
| **Citation** | *Not a node.* An immutable, content-addressed outside-proof attached to a Fact or an Abstraction ("I assert this because of that source"). | `cited_objects` + a `cited_<schema>` byte sidecar, linked to the memory by `citation_mappings` | attached at Fact ingest / at the Abstraction write |

So `memories` is **three** node kinds in one table (Fact, Abstraction,
Perspective), discriminated by `kind`; Goals are a separate axis; Citations
are bibliography hanging off a Fact or an Abstraction, never a node of their
own. The same
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
| A→P | `2^A × Π → P` | Abstractions become a typed Perspective under the active Perspective context. |
| frame | `P × A_cross → P` | Perspective may frame a cross-domain Abstraction. The frame is a Perspective whose payload references the Abstraction — never a standalone edge. Facts stay unchanged. |
| A→Goal | `2^A × Π → Goal` | Core derives / updates Goals from visible evidence (see 06). |

`Π` = active Perspective context selected by an authorized query or write.
Runtime identity is the Perspective memory row; type-level behavior comes from
the registered flavor. Load-bearing type evolution is a new flavor/type id;
load-bearing runtime lineage evolution is a new Perspective row whose own
declarations produce the index entries.

Forbidden:

- A→F, P→A, P→F writes.
- Upward F/A/P edges: Fact→Abstraction, Fact→Perspective,
  Abstraction→Perspective.
- A Fact as an interpretation source: an interpretation is a judgment, and a
  Fact asserts none. The layering rule already enforces it.
- Mutation of existing memories by later passes.

## Why This Layering — The Trauma Test

Rules:

- Facts are Reality observations. They are accepted, not revised.
- Abstractions are re-derivable interpretations over Facts.
- Perspectives are re-derivable integrations over Abstractions.
- Perspective context/type changes affect future derivations, not existing Facts.

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
| Abstraction | Typed `AbstractionPayload` sidecar + immutable `text` | Optional `citation_mapping_id` (see 11) | operator-authored | allowed |
| Perspective | Typed `PerspectivePayload` sidecar + immutable `text` | none — grounds through its references | operator-authored | allowed |

Facts are observations. Receipt-backed Facts come from source identities; receiptless
Facts are valid Fact writes without source-batch metadata. Fact identity is the
UUIDv7 `memory_id`, not the content hash or optional `receipt_id`.
`FactIngest` / `FactReceiptId` names are the current protocol vocabulary.

Abstractions and Perspectives are derived memories. Their provenance is
declared by the write that produced them (`derived_from`) and indexed as
`origin` entries, not stored as JSON inside the memory row. Their
reproducibility metadata lives inline on the memory row: operator kind, model
id, prompt version, the authoring Perspective (`authoring_perspective_id`), and
the declared inputs the `origin` entries index.

There is no `description` field. Facts render from payload. A/P text is
the authored cognitive surface. Typed sidecars are the query surface.

## Provenance

Provenance points downward, and it is an `origin` entry (see 16):

```
Perspective ──origin──► Abstraction ──origin──► Fact
```

Rules:

- F→A declares its Facts as `derived_from`; the write lands `Abstraction →
  Fact*` `origin` entries in its own transaction.
- A→P declares its Abstractions the same way, landing `Perspective →
  Abstraction*`.
- Cross-domain Fact synthesis is `Abstraction_cross → Fact*`.
- Perspective framing of cross-domain synthesis is a Perspective whose payload
  references `Abstraction_cross`.
- Bibliographic provenance for A/P is the transitive closure to Fact and
  Abstraction citations (see 11).
- Edges carry no citation id, no payload and no authorship column. Who
  reasoned is answered by the node that owns the statement — its
  `authoring_perspective_id`, its operator columns.

## Edges

Edges connect Memories and Goals, and they are an *index*, not a place content
lives. [16-edges.md](16-edges.md) is the reference for the model; this section
is the part a reader of 02 needs.

> An edge carries no information beyond its existence: its endpoints, its
> direction, its creation time, and its kind. All content lives in nodes;
> meaning arises from the synthesis of the connected nodes.

```
proxima_core.memory (
    origins uuid[],   -- made-from pins (not allowed on Facts)
    refs    uuid[],   -- points-at pins from payload references()
    ...
)
```

There is no edge id, so no edge handle: the row is its own identity, and
replaying a write re-asserts it instead of minting a duplicate. There is no
payload, no sidecar, no citation, no status, no relation, no namespace and no
authorship column.

## Kinds Are Closed

Two kinds, and the enum is not extensible — not by flavors, not by core
features. The kind is a *consequence* of the write that produced the row, never
a parameter a writer picks:

| Kind | Statement lives in | Written by |
|---|---|---|
| `origin` | the derived node's write (`derived_from`) | that write's own transaction |
| `reference` | the node's payload, as schema-declared reference fields | ingest / derivation, from payload content |

Two consequences follow, and they are what the rest of this document leans on:

- **Kind follows operation.** No public API takes an edge kind from a caller,
  and no verb writes an edge as a free-standing act. A feature that seems to
  need a third kind fails the node-home test and is missing a node.
- **Rebuildability.** Dropping the edge table and re-deriving it from node
  content yields the same set. This is the master invariant; every other edge
  guarantee is a corollary.

What used to be relation vocabulary now lives on the node that owns the
statement:

| Was | Now |
|---|---|
| `core/derived-from` | `origin`, from the write's `derived_from` |
| `core/supersedes` | `supersedes` / `superseded_by` pointers on the row |
| `core/authored` | `memories.authoring_perspective_id` |
| `core/inspires` | `goals.assignment_perspective_id` → one `reference` |
| `core/depends-on` | `goals.dependency_goal_ids` → one `reference` each |
| `core/motivated-by`, `core/wake-motivated-by` | `goals.evidence_memory_ids` → one `reference` each |
| `core/agent-link-refers-to` + its sidecar | an interpretation Perspective (`core_interpret`), whose subjects are payload references |

Multiplicity collapses: ten call sites from chunk A to chunk B are one index
row and ten entries in A's payload. The index answers "is there a connection";
the node answers "what it is".

## The Directionality Rule

Universal edge constraints, all enforced in storage (see 07):

- Endpoint ids must exist.
- Declared endpoint kind must equal stored endpoint kind.
- Edges are source-owned: `edge.owner == source.owner`.
- F/A/P layer rule: `ℓ(source) ≥ ℓ(target)`.
- Goal endpoints sit outside the F/A/P layer comparison.
- No self-loop: an edge cannot point at its own source.
- The source of an edge is the node that owns the statement — the derived node
  for `origin`, the referrer for `reference`.

Admission is one uniform rule, not a per-relation policy matrix: the write is
admitted iff the writer holds write authority on the source and read authority
on the target at write time. Since no verb writes an edge directly, that check
runs as part of the node write that declares it.

The endpoint's address form *is* its durable binding, so there is nothing to
configure: a Fact-entity-head endpoint follows the head as it is re-observed,
and a memory or Goal endpoint pins the row.

F/A/P matrix (`origin` and `reference` alike — the kind does not widen or
narrow it):

| From → To | Legal |
|---|---:|
| Fact → Fact | yes |
| Abstraction → Fact | yes |
| Abstraction → Abstraction | yes |
| Perspective → Fact | yes |
| Perspective → Abstraction | yes |
| Perspective → Perspective | yes |
| Fact → Abstraction | no |
| Fact → Perspective | no |
| Abstraction → Perspective | no |

The row that used to say "a Fact may not causally link a Fact" is now
structural: a causal claim is an interpretation, an interpretation is a
Perspective, and a Fact is never the source of one.

## Edge Scope Invariant

Edges are source-owned:

```
edge.owner == source.owner
```

The target may belong to a different Owner. That is what makes cross-owner
provenance expressible: an Abstraction owned by one group may ground itself in
another group's Fact while the edge remains owned by the Abstraction's source.
Supersession never crosses Owners, because it is not an edge at all — it is a
pointer on the row, and a row supersedes its own prior head.

Query visibility is not row ownership: source-readable edges may still redact
or suppress unreadable targets. A readable edge with an unreadable endpoint
comes back with the target withheld rather than the row suppressed, so the
existence of a connection is neither leaked nor hidden by accident.

## Causal Chain Query

Facts alone do not answer "why"; they only support correlation and
structure. Causal claims are Perspective-relative.

```
chain(f, P_active)
  = reference backbone among Facts
  + interpretation Perspectives under P_active, through their own references
  + origin closure from contributing P/A nodes to Facts
```

Rules:

- `chain(f, P_active)` is a query, not an entity.
- A causal claim is a node, not an edge kind: an interpretation Perspective
  whose payload references the memories it is about (`core_interpret`, see 12).
- Different active Perspectives can produce different valid chains.
- Supersession changes which P/A heads participate in future queries; old
  chains remain reconstructable from the append-only graph.
- A materialized chain view is a cache only, never authoritative.

## Wake / Dream / Write

Dreaming is flavor-declared consolidation through ordinary wake/write paths.
No Dream entity, Dream edge kind, or Core dream pipeline.

```
change_event
  -> armed Active Goal wake match
  -> actor/tool-scope admission
  -> typed Memory / Goal writes (edges follow from what they declare)
  -> registry + edge invariant enforcement
```

Wake configuration is Goal-owned (`Goal.wake`) and not a separate wake entity.

Dream forms:

| Form | Signature | Output |
|---|---|---|
| Compaction | `2^F × Π → A` | Abstraction |
| Reflection | `2^A × Π → P` | Perspective |
| Cross-domain synthesis | `2^F_cross × Π → A_cross` | Abstraction |
| Self/Perspective revision | `2^A × P_active × G_active → P_new` | Perspective |
| Goal reorientation | `P/A evidence → Goal write` | Goal write / supersession |

Dream outputs are ordinary writes. They obey schema registration, owner scope,
layer direction, citation rules, and append-only rules. They write no edge of
their own: what they declare produces the index entries.

## Re-derivation and Supersession

Facts never supersede and are never superseded.

A, P, and Goals may supersede. Supersession is **not an edge**: it is the same
thing persisting through revision, so it is a pair of pointers on the rows —
the successor's `supersedes` and the predecessor's `superseded_by`, both
written in the successor's own transaction:

```
new_entity.supersedes    = old_entity
old_entity.superseded_by = new_entity
```

Rules:

- Supersession is append-only: a new row plus the two pointers. No edge row is
  written for a supersession.
- Endpoint kind must match, and both rows share an Owner by construction —
  the pointer lives on the row, so it cannot cross an Owner boundary.
- Facts are never superseded (enforced in storage: `superseded_by` requires a
  non-NULL `kind`).
- Stateful Fact projections use head-by-natural-key queries on sidecars
  (see 03), not supersession.
- Deletion observations are Facts with state in their sidecar, not erased
  rows.
- Hard delete exists only as compliance erasure (see 13), outside cognitive
  graph semantics.

Default lineage scope is the owner plus the derived memory's `origin` entries.
Cross-context supersession is an explicit user/API editorial gesture, never an
operator decision.

## Assertion Lifecycle Pattern

Assertion = typed Abstraction whose sidecar carries a flavor-owned stable
key plus claim fields. Core owns lifecycle mechanics only.

```
Fact evidence*   --origin (declared as derived_from)--> Assertion(A)
Assertion(A_new) --supersedes / superseded_by pointers-> Assertion(A_old)
Assertion(A)     --reference (payload field)----------> Fact entity head*
```

Core requirements:

- assertion payload is an `AbstractionPayload` sidecar; no generic
  connection entity;
- evidence is declared as `derived_from`, landing `origin` entries; citations
  are Fact ∪ Abstraction (see 11);
- endpoint refs are ordinary schema-declared `references()` on the payload,
  preferably Fact-entity-head references for stateful entities, which follow
  the head;
- supersession writes `memories.supersedes` and the predecessor's
  `superseded_by` in the same transaction;
- current / superseded state is query-derived from heads, disposition, and
  flavor-owned validity fields.

Flavor responsibilities:

- stable assertion key shape;
- reference fields and payload fields;
- validity scope (`Date` interval, repo commit range, etc.);
- confidence / disposition enums;
- domain MCP wrappers and projection caches.

Do not add edge citation/status fields, an extensible connection vocabulary, a
core `RelationAssertion` entity, or authoritative materialized connection rows
for this pattern.

## Perspective context and wake

Perspective is a typed memory row. It may serve as an assignment, authorship,
or query context, but it is not an authz carrier. Server-resolved Owner roles
control reads and writes.

Substrate responsibilities:

- store typed Perspective rows and index their `origin` entries;
- store Goal-owned wake config for armed Active Goals;
- expose pull reads over `change_event`;
- enforce Owner roles, schema, and tool-scope gates;
- record produced A/P rows with their declared provenance;
- enforce registry and edge invariants.

Flavor responsibilities:

- prompt / instructions;
- Perspective schemas and default payloads;
- writeable schemas and the reference fields they declare;
- external harness decision policy.

Multiple Perspective contexts may be active for one Owner. Same Facts or
Abstractions under different contexts produce parallel lineages.

## What's Settled

- Strict F/A/P layering.
- Facts immutable; A/P/Goals append and may supersede.
- Cross-domain Fact synthesis is a typed Abstraction, not Fact→Fact semantics.
- A/P are always typed and always carry immutable text.
- Citations are Fact ∪ Abstraction and bibliographic (see 11).
- Edge kinds are a closed two-variant substrate enum; no verb writes an edge.
- Edge invariants are storage-enforced (see 07).
- The edge set is a function of node content — drop it and re-derive it, and
  it comes back the same.
- Supersession is a row pointer, not an edge.
- Causal chains and Self are queries, not entities (see 06).
- Dreaming is ordinary wake/write behavior, not a substrate component.

## Anchors

- `ontology-at-a-glance`
- `the-layering-principle`
- `why-this-layering-the-trauma-test`
- `the-core-entity`
- `provenance`
- `edges`
- `kinds-are-closed`
- `the-directionality-rule`
- `edge-scope-invariant`
- `causal-chain-query`
- `wake-dream-write`
- `re-derivation-and-supersession`
- `whats-settled`
