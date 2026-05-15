# 02 — Memory

## The layering principle

All memories are one of three kinds, with a **strict, irreversible
production order**:

```
                personality instance
                       │ wakes / decides / writes
                       ▼
   Facts ────────► Abstraction ────────► Perspective
          cross-domain Facts connect here, not as semantic F→F edges
```

Personality wakes (the arrows) produce higher-layer memories from lower:

- **F→A** — Facts → Abstraction. Biased by personality (Perspective + Goals).
- **A→P** — Abstractions → Perspective. Biased by full personality (Q4).

Perspective may additionally **frame cross-domain Abstractions**. The Facts
are unchanged; semantic or causal Fact-to-Fact edges are forbidden. The legal
cross-domain channel is `A_cross -> F*`, optionally framed by `P -> A_cross`.

Forbidden: any wake that lowers layer (A→F, P→A, P→F); any edge that
references a higher layer (F→A-edge, F→P-edge, A→P-edge). Full edge
directionality table under §Edges.

## Formalization

Sets:

- **F** = { m ∈ Memory | kind = Fact }
- **A** = { m ∈ Memory | kind = Abstraction }
- **P** = { m ∈ Memory | kind = Perspective }

Layer function `ℓ : Memory → {0, 1, 2}` with `ℓ(F)=0, ℓ(A)=1, ℓ(P)=2`.

Personality `Π = (personality_type_id, personality_instance_id)`.
Each instance has a self-Perspective and wake config; see §Personality
and [08](docs/08-core-and-flavors.md).

Multiple personality instances may be active per Owner. Each wake runs
under one instance at a time; parallel personalities produce parallel
A/P lineages tagged by the split personality identity.

Wake/write paths are indexed by Π:

| Operator | Signature | Restriction |
|---|---|---|
| F→A   | `2^F × Π → A`      | `S ⊆ F`; intra-source by default, cross-domain only when the output schema is an explicit cross-domain Abstraction. Multiple personalities may produce distinct typed Abstractions over the same Facts. |
| A→P   | `2^A × Π → P`      | `S ⊆ A` visible to the personality instance; plural personalities allowed |
| frame | `P × A_cross → Edge` | optional Perspective authorship/framing of a cross-domain Abstraction |

Edge constraint: for every edge `e : m_s → m_t`, `ℓ(m_s) ≥ ℓ(m_t)`.

**Stepping between layers.** Π biases wakes *downward*: a self-Perspective
or Goal change reshapes future F→A / A→P outputs and authorises new framed
Abstractions over existing F. Personality outputs propagate *upward* via new memories
and supersession edges; never by mutation.

**Causa proxima.** A "why" answer for `f ∈ F` is a path through `A/P` for
some `p ∈ P_active`, justified by `prov(p) ⊆ A` and the Fact provenance of
each Abstraction. Not extractable from F alone: F yields correlation in time;
the causal chain is A/P-mediated.

## Why this layering — the trauma test

> Self cannot change Facts, only the Perspective about them. Trauma is
> resolved by accepting the real Facts and rebuilding Perspective above them.

Consequences (load-bearing on the entity shape):

1. **Facts immutable.** Once recorded from an Event, a Fact stays.
2. **Abstractions re-derivable.** Same F-subset under a different Π yields
   a new Abstraction. Both coexist; older has older provenance.
3. **Perspectives evolve.** A Perspective is superseded by a new
   Perspective derived from a richer A-set or under a different Π.
4. **Healing = re-run wake under updated Π.** A "traumatic" Abstraction is
   one produced under a Π that didn't fit the Facts. Update Π, re-run.

## The core entity

All three kinds share the same row. Content storage is *kind-appropriate*:
Facts in a typed sidecar (always, no `text`); Abstractions and Perspectives
both in a `text` column **and** in a typed sidecar (always — every A and
P registers a schema; see [03 §AbstractionPayload / PerspectivePayload](docs/03-schema-registry.md#abstractionpayload-and-perspectivepayload)).

```rust
struct MemoryRecord {
    // Shared across every kind, declared once.
    id:         MemoryId,                   // UUIDv7
    owner:      Owner,                      // scope (see [01](docs/01-event-source.md))
    schema_id:  SchemaId,                   // NOT NULL for every memory
    created_at: Timestamp,
    body:       MemoryBody,
}

enum MemoryBody {
    Fact {
        event_id:            EventId,
        citation_mapping_id: CitationMappingId,
    },
    Derived {
        kind:                   DerivedKind,      // Abstraction | Perspective
        text:                   String,
        operator_kind:          OperatorKind,     // Wake
        model_id:               ModelId,
        prompt_version:         PromptVersion,
        personality_type_id:    PersonalityTypeId,
        personality_instance_id: PersonalityInstanceId,
        wake_chain_depth:       WakeChainDepth,
    },
}

enum DerivedKind { Abstraction, Perspective }
enum OperatorKind { Wake }
```

Storage layout for this entity is defined in [07-storage.md](docs/07-storage.md).

`event_id` carries the Fact-side dedup constraint (FK to `events`,
unique among Facts). `id` is a fresh UUIDv7 for every memory regardless
of kind — Fact identity is not the content hash, the FK to the source
event is.

`citation_mapping_id` is **bibliographic only**. Full model in
[11-citations.md](docs/11-citations.md). A/P memories have no direct
citation; their bibliographic provenance accumulates by walking
provenance edges down to Facts.

Invocation columns (`operator_kind`, `model_id`, `prompt_version`,
`personality_type_id`, `personality_instance_id`, `wake_chain_depth`)
are inline reproducibility metadata for A/P (see
[04 §Idempotence](docs/04-consolidation.md#idempotence-and-reproducibility)).
The split personality identity keeps parallel personalities' outputs on
disjoint invocation keys: the same `(model, prompt)` under two different
instances never collides on dedup.

A Fact's `owner` equals its source Event's `owner`. Abstractions and
Perspectives inherit `owner` from their input memories — personality
wakes run within one owner; outputs share that owner.

The enum design eliminates field duplication: `id`, `owner`, `schema_id`,
`created_at` exist once with direct field access (`m.id`, `m.owner`).
Variant-specific fields stay variant-typed — match on `body` to access
them. The persistence boundary is a one-to-one projection: shared
columns + a kind-discriminated tail.

Version is implicit in sidecar table membership — a row in
`fact_forgejo_commit_v3` is by definition at version 3.

There is **no `description` field**. A memory does not describe itself or
anything else; it *is* what it is.

- A **Fact** is its typed payload — a structured observation from Reality,
  stored in `fact_<schema>_v<n>` joined by `memory_id`. Text is rendered
  on-demand from the payload (`FactPayload::render`). Nothing is stored.
  Version is implicit in the sidecar table name.
- An **Abstraction** is its `text` — a paragraph of synthesised
  understanding, written by the F→A operator at creation. Immutable.
  Every Abstraction also carries a typed sidecar row in
  `abstraction_<schema>_v<n>` — the **queryable scaffolding** (selective
  extraction, see [03](docs/03-schema-registry.md)). `text` is the playing space; the sidecar is the
  query surface. Version is implicit in the sidecar table name.
- A **Perspective** is its `text` — a paragraph of integrated meta-model,
  written by the A→P operator at creation. Immutable. Same shape as
  Abstraction: required typed sidecar alongside `text`. Version is
  implicit in the sidecar table name.

Dreams do not "enrich" existing memories. Dreams produce *new* memories
(Abstractions, Perspectives) whose provenance edges reference the source
memories they were derived from. Memories never get rewritten by later passes.

A Fact's `citation_mapping_id` points to a typed `CitationMapping`
→ `CitedObject` (see [11-citations.md](docs/11-citations.md)). Abstractions and
Perspectives have no citation_mapping_id; their bibliographic provenance
is the transitive closure over provenance edges into the Fact layer.

Reproducibility metadata for A/P is inline on the memory row, not a citation.
Provenance — *which* source memories produced this Abstraction or
Perspective — is carried by **edges**, not by JSON inside the Memory row.

## Operator scope

| Operator | Scope | Input | Why |
|---|---|---|---|
| **F→A** | intra-source by default; explicit cross-domain Abstractions allowed | Facts selected by the operator; cross-domain input requires an output schema that names the synthesis | local understanding first; domain blending is represented by a typed Abstraction, never a semantic Fact-to-Fact edge. Multiple F→A operators may run in parallel over the same Facts, each producing a different typed Abstraction. |
| **A→P** | intra-flavor or cross-flavor | Abstractions retrieved by similarity / relevance, scoped per the operator's declaration | personalization; weaving disparate sources is what makes a Perspective *this agent's*; multiple A→P operators may run in parallel and produce different typed Perspectives |

A **source batch** is the set of Facts emitted as a single observation
event from an Event Source. `source_batch_id` is a **UUIDv7 declared
opaquely by the source** at emit time; the engine validates uniqueness
within `(source_id, owner)` and rejects collisions. Sources already
control observation grouping — letting them set the id directly drops
one piece of determinism API.

Cadence: each operator declares its own predicate (batch-close,
goal-write, schedule, abstraction threshold). F→A typically fires on
batch-close; A→P operators choose. Engine evaluates predicates and
dispatches (04 §Phase 2).

## Edges

Edges connect any pair of addressable entities in the graph — Memories
*or* Goals. The four-pillar ontology (universe.md) puts Goals outside
the F/A/P layering as their own axis ([06](docs/06-goals-and-self.md)); the edge graph therefore
addresses them with a sum type.

```rust
enum EntityId {
    Memory(MemoryId),
    Goal(GoalId),
}

enum EntityKind {
    Fact,
    Abstraction,
    Perspective,
    Goal,
}

struct Edge {
    id:           EdgeId,
    source_id:    EntityId,
    target_id:    EntityId,
    relation:     Relation,
    authored_by:  Authorship,    // the reasoning concept; edges do not cite (see [11](docs/11-citations.md))
    created_at:   Timestamp,
}

struct Relation { id: RelationId }   // descriptor-keyed; see §Relation registry

enum Authorship {
    // Phase-1 / payload-derived: edges that fall out of an event payload by construction.
    EventSource(SourceId),

    // Phase-2 / operator-derived: edges authored by an interpretive operator.
    OperatorFtoA(MemoryId),         // provenance A→F; owned by the source Abstraction
    OperatorAtoP(MemoryId),         // provenance P→A; owned by the source Perspective
    OperatorAtoGoal(GoalId),        // provenance Goal→A; owned by the produced Goal (04 §A→Goal)
    PerspectiveLink(MemoryId),      // P-authored framing edge; never semantic F→F

    // Core / infrastructure: supersession and explicit user edits; not flavor-pluggable.
    Core(CoreAuthor),
}

enum CoreAuthor {
    Engine,            // engine-authored: supersession (P→P, A→A, Goal→Goal)
    User(UserId),      // user-authored via API: Goal parents, future user edits
}
```

`MemoryBody` (Fact / Derived with Abstraction | Perspective) is the
discriminator on the `MemoryRecord` row; `EntityKind` is the strict
superset used by the relation registry and edge endpoint typing.

**Ownership rule.** Within F/A/P, cross-set edges are **top-down**: the
higher-layer endpoint owns the edge. So a P→A edge is owned by its
source P, an A→F edge by its source A, a P→F edge by its source P.
Within-set edges (F→F, A→A, P→P) are trivial — owned by whoever
authored them in that context (`EventSource` for structural payload
edges, `Core(Engine)` for P→P and A→A supersession). Semantic and causal
F→F edges are not legal. No edge is ever co-owned by both endpoints.

For Goal-involved edges, ownership is determined by the producing
operator, source, engine, or user API rather than by layer comparison
(Goals sit outside the F/A/P layering): `OperatorAtoGoal(g)` for
A→Goal-derived provenance, `EventSource(SYSTEM)` for tool-emitted
Action-Fact → Goal motivation hints, `PerspectiveLink(p)` for P-mediated
motivation, `Core(Engine)` for Goal→Goal supersession, `Core(User(u))`
for user-authored Goal parents.

Authorship has three structural classes: Phase-1 (payload-derived via
`EventSource`), Phase-2 (operator-derived via `Operator*` and
`PerspectiveLink`), and Core (infrastructure: `Engine` and `User`).
Operator variants are flavor-pluggable; Core variants are built-in.
`Engine` and `User` authorship are valid for any edge whose
`RelationDescriptor` masks permit them; they are not bound by the
layer comparison rule.

### Relation registry

Every `relation.id` must resolve to a `RelationDescriptor` registered by
some flavor (or by core, for cross-cutting relations like
`core/motivated-by`, `core/derived-from`, `core/parent`,
`core/supersedes`) at startup. Engine rejects edges with unregistered
relations. `core/supersedes` is registered with a kind mask that
permits A→A, P→P, and Goal→Goal only — F→F supersession is not
expressible at the edge layer (see §Re-derivation and supersession).

```rust
struct RelationDescriptor {
    id:                RelationId,         // flavor-qualified, e.g. "code/commit-fixes"
    class:             RelationClass,
    source_kind_mask:  EntityKindMask,     // which EntityKind values may be source (incl. Goal)
    target_kind_mask:  EntityKindMask,     // which EntityKind values may be target (incl. Goal)
    authorship_mask:   AuthorshipMask,     // which Authorship variants may author it
    payload_schema:    Option<SchemaRef>,  // some(EdgePayload (id, version)) iff edges of this relation
                                           // carry a typed sidecar; None for substrate-only relations
}

enum RelationClass {
    Structural,    // EventSource-authored from payload (commit→parent_commit, …)
    Provenance,    // operator-authored A→F or P→A
    Supersedes,    // engine-authored on re-derivation
    Causal,        // PerspectiveLink — causa proxima carrier
    Interpretive,  // PerspectiveLink — non-causal interpretation
}
```

`RelationClass` is **closed by design**. The A/P-traversal contract
(causal-chain queries, scope filtering, supersession) needs a fixed
substrate vocabulary; flavors pick the class their relation logically
falls under, then differentiate via the `relation` string. New
flavor-specific edges add `RelationDescriptor` entries, never new
`RelationClass` variants.

Registered via `reg.add_relation(descriptor)` in the flavor's `register`
call (see [08](docs/08-core-and-flavors.md)). The descriptor is the source of truth for what edges are
legal; the directionality rule below is the *minimum* additional check.

### Typed edge payloads

Every edge has a substrate row in `proxima_core.edges` keyed by
`edge_id: uuidv7`, carrying the discriminators (`relation`,
`relation_class`), endpoints, authorship, owner, and timestamps.
A relation registered with `payload_schema = Some(...)` additionally
writes a typed sidecar row in the flavor-owned table named by the
`EdgePayload` impl ([03 §EdgePayload](docs/03-schema-registry.md#edgepayload));
the sidecar is keyed on `edge_id` and joined identically to memory
sidecars.

The atomic write verb that ingests Facts (and the F→A consolidation
verb) extends to typed edges: substrate edge row plus zero-or-one
`EdgePayload` sidecar in the same transaction. Substrate-only
relations (`core/derived-from`, `core/supersedes`, `core/parent`,
`core/motivated-by`) leave `payload_schema = None` and skip the
sidecar insert — same row in `proxima_core.edges`, no flavor table
involved.

**Edges are immutable in v1.** When a Fact endpoint is superseded
(stateful Fact head moves to a new memory under the same natural
key — see [03 §Stateful Fact schemas](docs/03-schema-registry.md#stateful-fact-schemas--head-by-natural-key)),
the producing operator authors fresh edges against the new memory;
old edges remain valid for the old memory. Edge-level
head-by-natural-key — "rebind the `calls` edge when the callee
chunk is rewritten" — is a focused future milestone, only when a
flavor demands it. Until then, supersession lives entirely at the
memory layer.

### The directionality rule

> Within F/A/P, an edge from layer `m` to layer `n` is permitted iff
> `m ≥ n`. Goal-involved edges sit outside this rule and are governed
> by the relation descriptor's kind masks alone. Authorship classes
> are the producing operator, source, engine, or user API.

Within F/A/P:

| From → To | Allowed? | Typical class | Authorship |
|---|---|---|---|
| Fact → Fact | ✓ | `Structural` / `Provenance`; **never `Causal`, `Interpretive`, or `Supersedes`** | `EventSource`, `Engine`, `ExternalAgent` |
| Abstraction → Fact | ✓ | `Provenance` | `OperatorFtoA` |
| Abstraction → Abstraction | ✓ | `Structural` / flavor-specific | `Core(Engine)` for supersession |
| Perspective → Fact | ✓ | `Causal` / `Interpretive` | `PerspectiveLink` |
| Perspective → Abstraction | ✓ | `Provenance` | `OperatorAtoP` |
| Perspective → Perspective | ✓ | meta-level / `Supersedes` | `Core(Engine)` for supersession |
| Fact → Abstraction | ✗ | Facts don't reference higher layers. | — |
| Fact → Perspective | ✗ | Same. | — |
| Abstraction → Perspective | ✗ | Forbidden by strict layering. | — |

Goal-involved (Goal sits outside F/A/P; descriptor masks govern):

| From → To | Allowed? | Typical class | Authorship |
|---|---|---|---|
| Goal → Abstraction | ✓ | `Provenance` | `OperatorAtoGoal(g)` ([04](docs/04-consolidation.md)) |
| Goal → Goal | ✓ | `Structural` (parent) / `Supersedes` | `Core(User(u))` for parent, `Core(Engine)` for supersession |
| Fact → Goal | ✓ | `Causal` (`core/motivated-by`) | `EventSource(SYSTEM)` for tool-emitted Action-Facts |
| Perspective → Goal | ✓ | `Causal` (`core/motivated-by`) | `PerspectiveLink(p)` for P-mediated motivation |
| Goal → Fact / Perspective | — | only as registered; no built-in core relation | — |

The F/A/P layer rule is **hardcoded**: it falls out of the operators'
set signatures (F→A : 2^F × Π → A; A→P : 2^A × Π → P; framing :
P × A_cross → Edge). No registered relation can permit an upward F/A/P
edge — descriptor masks tighten the rule per relation, never relax it.
Database triggers join edge endpoints to stored endpoint kinds for the
storage-level guarantee and reject direct `Causal` or `Interpretive` F→F.
For Goal endpoints the trigger validates endpoint truth; the F/A/P layer
comparison short-circuits when either endpoint is Goal.

### Edge scope invariant

`source.owner == target.owner` for every edge — including Goal
endpoints, since Goals carry `Owner` (06). Cross-owner edges are
rejected. Sharing across owners (v2+) is a query-layer concern via
the `AccessGrant` extension (see [01](docs/01-event-source.md)); it does not write cross-owner
edges.

## Causal chain query

A "why" answer for `f ∈ F` is a query, not an entity. Defined parallel
to how Self is "a pure query" (Q5 / 06):

```
chain(f, P_active) = transitive closure over Edge starting from f, where:
    - relation.class ∈ {Causal, Structural}
    - authorship ∈ {EventSource, PerspectiveLink(p) for p ∈ P_active}
returning the DAG of (Memory, Edge) plus the A-provenance subtree of
every contributing Perspective (P → A → F via Provenance edges).
```

Properties:

- **Π-relative.** Different `P_active` → different chains over the same
  Facts. Causa proxima is a *modeled* answer, not an objective one.
- **Append-only-safe.** A chain query never writes; supersession of a
  Perspective shifts which edges count for `P_active` without
  invalidating prior chains' citations.
- **No new entity.** A materialised view is permitted as a perf cache,
  never authoritative. The graph is the source of truth.

Chains compose with relation-class filtering: ask `chain(f, P_active)`
restricted to `RelationClass::Structural` for the EventSource-only
backbone (no interpretation), or to `RelationClass::Causal` for the
Perspective-mediated explanations (no plain succession).

## Wake / decide / write

`PersonalityFlavor` is the decider unit (08). Each runtime instance is
addressed by `(personality_type_id, personality_instance_id)`, owns a
self-Perspective, and stores wake filters in `personality_wake_config`.

```rust
change_event
  -> wake filter match
  -> PersonalityFlavor::decide(ctx)
  -> typed Abstraction / Perspective writes
  -> Provenance / Supersedes edges
```

Layer targets remain strict:

| Write kind | Input set | Output |
|---|---|---|
| Fact-triggered wake | Facts sharing one source batch | Abstraction |
| Abstraction-triggered wake | Abstractions visible to the instance | Perspective |
| Goal / edge-triggered wake | Goal or `core/inspires` edge to self-Perspective | Perspective / Goal response |

Same Facts or Abstractions under different personality instances produce
parallel lineages. Each output carries the split personality identity;
supersession is scoped to the same `(type_id, instance_id)` unless a
user/API-authored editorial action explicitly crosses instances.

Reproducibility columns on A/P rows are `(model_id, prompt_version,
personality_type_id, personality_instance_id, wake_chain_depth)`. The
trigger and read provenance are recorded as edges.

## Re-derivation and supersession

**Supersession applies to A, P, and Goals only.** Facts have a stricter
contract: each Fact is one observation at one time, immutable at the
identity layer, never replaced and never linked to a successor. The
Fact layer therefore registers no `Supersedes`-class relation; the
F→F directionality row in the table above admits only mechanical
`Structural` / `Provenance` edges.

Stateful Fact projections — "current revision of file X", "latest
snapshot of resource Y" — are expressed at **query time** as
head-by-natural-key over the schema's sidecar (latest by natural-key
columns ordered by `observed_at` desc, limit 1), not as a `supersedes`
chain. Deletion observations are themselves Facts under a
`state: Tombstone` enum field on the sidecar — same schema, different
state. The pattern and its sidecar contract live in
[03 §Stateful Fact schemas](docs/03-schema-registry.md#stateful-fact-schemas--head-by-natural-key).

Memories are **never destroyed**. When a new Abstraction is derived from
the same Facts under the **same** `personality_id`:

1. The old Abstraction stays. Its provenance edges point to the same Facts
   under the older invocation.
2. The new Abstraction is added. Its provenance edges also point to those
   Facts, but under the newer invocation.
3. The two are linked by a `supersedes` edge: `new_abstraction →
   old_abstraction`, authored by `Core(Engine)`.

Consumers querying "current state" follow `supersedes` chains to the head;
consumers querying history walk the chain backward.

**Supersession is intra-personality by default.** Re-deriving under a
different `personality_id` produces a *parallel* lineage, not a
supersession — an Abstraction authored by a personality with one
self-Perspective (e.g., a hypothetical "Stoic Visionary" personality) over
Fact `f` does not supersede an Abstraction authored by a personality with a
different self-Perspective (e.g., "Workhorse Programmer") over the same
`f`. They coexist as parallel interpretations. (Names like "Stoic
Visionary" and "Workhorse Programmer" here are user-chosen labels, not
engine archetypes; see
[`docs/superpowers/specs/2026-05-10-personality-vocabulary-and-archetype-discipline.md`](superpowers/specs/2026-05-10-personality-vocabulary-and-archetype-discipline.md).)

**Cross-personality supersession is an editorial gesture** — "this
voice replaces that one across the agent's identity." It is reserved
for `Core(User(u))` authorship; operators do not get to make
cross-personality editorial decisions. When a personality flavor is
retired from a binary, its existing rows stay readable (orphaned by the
unlinked operator); a new active personality may explicitly supersede
them via user-API write. See §Read-scope matrix for how parallel
lineages are addressable at query time.

This mirrors the supersession pattern already in hippocampus.

**Hard delete is an out-of-band invariant break.** The
"memories are never destroyed" rule above is the *cognitive*
contract — agents do not forget; the substrate revises only via
supersession. Subject-rights regimes (GDPR, UK GDPR, CCPA, LGPD,
…) require operations that *do* destroy memory, scoped to a
specific Owner and audited separately from the cognitive graph.
The substrate ships those operations as a deliberate, auditable
break of the invariant; cognitive readers (operators, deciders)
observe the diminished graph as if the deleted entries had never
existed. See [15](15-compliance.md) for the operations
(`delete_owner`, `pause_owner`, `export_owner`, …), the
suppression-list mechanic that makes erasure re-ingest-safe, and
the `compliance.*` audit schema that proves erasure happened.

## Personality

**Personality is a flavor-declared decider type plus runtime instances.**
The substrate ships the trait shape, wake storage, and dispatcher; prompts,
self-schema, tool palette, writeable schemas/relations, wake entries, tier,
and capability requirements live in registered `PersonalityFlavor` impls
(see [08 §Registration mechanism](docs/08-core-and-flavors.md#registration-mechanism)
and [13 §Authoring contract](13-flavor-marketplace.md#authoring-contract)).

```rust
trait PersonalityFlavor {
    fn personality_type_id(&self) -> &'static str;
    fn self_schema(&self) -> SchemaId;
    fn default_self_payload(...) -> PersonalitySelfDraft;
    fn system_prompt(&self) -> &'static str;
    fn tools(&self) -> Vec<Arc<dyn PersonalityTool>>;
    fn writeable_schemas(&self) -> &'static [&'static str];
    fn writeable_relations(&self) -> &'static [&'static str];
    fn tier(&self) -> ModelTier;
    fn max_wake_chain_depth(&self) -> u16;
    fn requires(&self) -> LlmCaps;
    async fn decide(&self, ctx: PersonalityDecisionContext<'_>) -> PersonalityDecision;
}
```

The substrate records `(personality_type_id, personality_instance_id)`
inline on every produced A/P row. Load-bearing type evolution is a new
`personality_type_id`; runtime branching is a new `personality_instance_id`
with its own self-Perspective and wake entries.

**Selection primitives** a `PersonalityFlavor` typically composes:

| Primitive | Cost | Use |
|---|---|---|
| Identity Perspectives    | filter by schema flag      | always-active foundational views |
| Recency window           | O(window)                   | floor when other channels are thin |
| Topical (similarity)     | embedding pass at snapshot  | retrospect — what the agent already thinks here |
| Goal-relevant            | filter by Goal-edge citations | intent — what supports the agent's current ambitions |

These are *primitives*. A flavor decides which to combine and with what
top-K caps. The substrate does not pick a canonical combination because there is
no canonical answer — different personality flavors are *exactly* the
business of having different combinations.

**Multiple personality instances per Owner.** Many personality types may
be linked into one binary, and each Owner may instantiate multiple
instances of a type. The dispatcher runs per active wake-config row; a
single event may produce parallel A/P sets across instances. The
substrate hosts plural selves; the agent runs a topology over its own
memory. Identity is configurable.

## Read-scope matrix

When multiple personalities are active per Owner, retrieval needs an
explicit answer for cross-personality reads. The substrate provides
this via a **per-Owner boolean adjacency matrix** over linked
personalities:

```
M[self][other] = 1   →  `self` can retrieve `other`'s A/P (and Goals) as input
M[self][other] = 0   →  retrieval excludes `other`'s outputs
```

Invariants:

- **Identity diagonal.** `M[p][p] = 1` for every linked personality `p`,
  always. A personality's own outputs are always readable to itself.
- **F is below the matrix.** Facts are objective; every personality
  always sees every Fact. The matrix governs the interpretive layer
  (A, P, and Goals derived under a personality) only.
- **Asymmetry is a feature.** `M[a][b] = 1 ∧ M[b][a] = 0` is well-formed
  and load-bearing — e.g. a Synthesist that integrates everyone's views
  without those views being polluted by Synthesist's meta-output.
- **Direct reads only.** The matrix governs *direct* retrieval scope;
  transitive influence happens through the graph naturally and is not
  re-checked at read time. A Perspective written by `S` that synthesises
  `W`'s view is a memory authored by `S`; readers of `S` see it via
  `M[reader][S]` regardless of `M[reader][W]`.
- **Append-only-safe.** Toggling `M[a][b]` from 0 to 1 changes future
  retrieval scope for `a`, but does not alter or invalidate `a`'s
  existing memories. Re-deriving `a`'s A/P under the new matrix is an
  explicit re-run.

The matrix is per-Owner config; storage lives in
[07 §Core tables](07-storage.md#core-tables-abstract) (`read_scope_matrix`).
Default for newly-linked personalities is the identity row + identity
column only (each new personality starts isolated; cross-reads opt in).

The matrix affects future retrieval scope. If a matrix change is
load-bearing enough to require disjoint operator lineage, register a
new `personality_id`; the substrate has no in-place personality-state
key.

## Sub-questions — all resolved

### Q1. Typed Fact payloads via build-time schema registration

Facts carry typed payloads via `FactPayload` impls registered at build
time through `proxima_flavor!` ([08](docs/08-core-and-flavors.md)). The system materialises per-schema
sidecar tables. See 03.

### Q2. Facts share the core entity; content storage is kind-appropriate

Shared `Memory` row carries `id` (UUIDv7), `owner`, `kind`,
`schema_id` (NOT NULL for every memory), and timestamps. Version is
implicit in sidecar table membership. Facts also carry `event_id`
(FK to `events`) and `citation_mapping_id` (FK to `citation_mappings`,
see [11](docs/11-citations.md)), with `text = NULL` and operator-invocation
columns NULL. Abstractions and Perspectives have `event_id = NULL`,
`citation_mapping_id = NULL`, non-null operator-authored `text`, a typed
sidecar row, and inline operator-invocation reproducibility columns.
No `description`. Renderer on-demand for Facts; immutable text for
A and P. Dreams produce new memories, never rewrite.

### Q3. Strict layering — no upward edges from any layer

Resolved: **strict**. Facts cannot link to anything above. Abstractions
cannot link to Perspectives. Perspective → Abstraction is provenance only;
the reverse is never authored. Matches the trauma invariant (Reality is
fixed; layers above derive from below, never the other way). Enforceable
as a check constraint joining `edge.source.kind` and `edge.target.kind`.

### Q4. A→P biased by full personality

Resolved: **full personality instance** — Π is the runtime
`(personality_type_id, personality_instance_id)` plus its current
self-Perspective, wake config, readable memory scope, and active Goals.
Both F→A-like and A→P-like writes are produced by the same
`PersonalityFlavor::decide` path. Reason: perspective evolution needs
continuity with prior Perspective state or each run is computed from
scratch, breaking the "perspectives shift slowly under personality
drift" pattern. Stuck-loop risk is mitigated by self-exclusion,
`wake_chain_depth`, Goal updates, new Facts, and new personality
instances.

What "full personality" *includes* is the flavor's call. The substrate
guarantees only the instance identity, self-Perspective anchor, wake
filters, read/write authorization, and lineage fields.

### Q5. Self as flavor projection — see [06](docs/06-goals-and-self.md)

Resolved: **self-Perspective anchored query**. No Self entity, no cache.
Different personality instances project different Selves over the same
memory graph; a binary's composition determines which Selves it can
present. Full treatment in
[`docs/06-goals-and-self.md`](06-goals-and-self.md).

## What's settled

- Strict Facts → Abstraction → Perspective layering, no exceptions
  (Q3).
- Single core `Memory` entity, all three kinds. UUIDv7 ids; Facts
  carry an `event_id` FK to `events` for re-receipt dedup. Typed
  payloads on every memory via the schema registry ([03](docs/03-schema-registry.md)). Owner per
  memory ([01](docs/01-event-source.md)).
- **Citations are bibliographic and Fact-only.** Full model in
  [11-citations.md](docs/11-citations.md). A/P bibliographic provenance
  accumulates by walking provenance edges to Facts. Edges have **no**
  `citation_id`; `authored_by` carries the reasoning concept.
- Edge directionality: outbound only to same or lower layer; same-Owner
  invariant.
- Wake/decide/write is the production path. A personality may write
  typed Abstractions or Perspectives within its declared writeable
  surface. Both are biased by full personality instance (Q4).
- **Personality is a flavor-declared type plus runtime instances.**
  `PersonalityFlavor` declares type id, self-schema, prompt, tools,
  writeable surface, wake filters, tier, and `decide`. The substrate
  hosts plural personalities per Owner; A/P rows tag with
  `(personality_type_id, personality_instance_id)`; parallel
  personalities yield parallel lineages, never collide on supersession.
  Cross-personality supersession requires user-API authorship.
- **Read-scope matrix.** Per-Owner boolean adjacency over personalities
  governs cross-personality retrieval. Identity diagonal = 1; F always
  shared; A/P/Goals gated. Load-bearing matrix evolution requires a new
  `personality_type_id`.
- Memories immutable in Facts and additive elsewhere; supersession links
  new derivations to old, scoped within personality.
- Self is a flavor projection (Q5 → 06).
- **Relations are typed and flavor-registered** via
  `RelationDescriptor`. Engine rejects edges with unregistered
  `relation.id`; Phase-1 EventSource edges obey the same registry
  (see [04](docs/04-consolidation.md)). Built-in relation classes: `Structural`, `Provenance`,
  `Supersedes`, `Causal`, `Interpretive`.
- **Abstractions and Perspectives are always typed** via
  `AbstractionPayload` / `PerspectivePayload` (03). Typing is
  required per memory and **selective** — the typed payload captures
  queryable scaffolding only, `text` remains the playing space. No
  JSON escape hatch; required fields only. Build-time registration
  only — no runtime schema-registration API at any tier.
- **Causa proxima is a query**, not an entity:
  `chain(f, P_active)` returns the Π-relative DAG over `Causal` and
  `Structural` edges plus the contributing P/A provenance subtree.
  Materialised view permitted as a perf cache, never authoritative.

## Anchors

- `the-layering-principle`
- `formalization`
- `why-this-layering-the-trauma-test`
- `the-core-entity`
- `operator-scope`
- `edges`
- `relation-registry`
- `the-directionality-rule`
- `edge-scope-invariant`
- `causal-chain-query`
- `the-operators`
- `f-to-a-fact-to-abstraction`
- `a-to-p-abstraction-to-perspective`
- `re-derivation-and-supersession`
- `personality`
- `read-scope-matrix`
- `sub-questions-all-resolved`
- `q1-typed-fact-payloads-via-build-time-schema-registration`
- `q2-facts-share-the-core-entity-content-storage-is-kind-appropriate`
- `q3-strict-layering-no-upward-edges-from-any-layer`
- `q4-a-p-biased-by-full-personality`
- `q5-self-as-flavor-projection-see-06`
