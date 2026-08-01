/-
Causa — Identity

ID types and the append-only storage discipline (doc 07). The storage rule in one
line (doc 07 §Append-Only): INSERT is the normal write path; UPDATE
is not part of the cognitive entity lifecycle; DELETE exists only as
compliance erasure (ST-11, ST-13 — see Causa.Compliance).

Identity rules (doc 07 §Identity Rules):
  - Fact / Abstraction / Perspective: fresh UUIDv7 MemoryId.
  - Goal: fresh GoalId; supersession writes a new row.
  - Edge: insert-only, and it has NO id at all. The index row is its own
    identity — the primary key is `(source, target, kind)` — so there is no
    `EdgeId` type here, no content-hash arm, and nothing to mint. That
    ABSENCE is E5 (doc 16 §The edge table is an index); the v0.0.7
    identity-hash scheme existed to approximate what the row now has by
    construction.
  - Source/flavor ingest deduplication is metadata around typed Facts,
    not a separate core Event entity.
  - Embeddings are engine-side: the kernel models no `Embedding` entity,
    and the deliberate absence of a `Memory → Embedding` accessor IS the
    vector-store-independence invariant (ST-15..17).
-/

import Causa.Prelude
import Causa.Owner

namespace Causa

-- ============================================================
-- Identity — ONE opaque token (doc 07 §ID Types)
-- ============================================================

/-- The one identity primitive: a fresh, unique, engine-minted token (UUIDv7).
    EQUALITY is all the kernel asks of it — for row uniqueness and for id-pointers
    (e.g. Goal supersession). Its 128-bit layout and v7 time-sortability are
    engine commitments, deliberately NOT modeled: the kernel's time axis is
    `Memory.created_at : Instant`, never the id (two clocks would be a hazard),
    and a concrete UUIDv7 would expose ordering/arithmetic the kernel never uses.
    Opaque declaration, not axiom, is the faithful boundary: Causa proofs use
    only equality and never inspect the hidden representation.

    This opacity rule remains law for row ids, operator ids, input contracts,
    and schema refs. `User` is defined separately in `Owner.lean` under the
    owner-algebra token exception (private representation, public mint +
    equality).

    The kernel needs NO cross-type id distinctness (no theorem reads it), so the
    per-entity id names are documentation abbrevs over this single type rather
    than separate opaque types. -/
opaque Id : Type := String

abbrev MemoryId          : Type := Id
abbrev GoalId            : Type := Id
/-- Stable handle for a stateful Fact aggregate. It is a fresh engine-minted
    token, NOT a content hash and NOT Fact identity; Fact identity remains the
    current version's `MemoryId`. -/
abbrev FactEntityId      : Type := Id
abbrev CitedObjectId     : Type := Id
abbrev CitationMappingId : Type := Id

/-- Stable persisted owner-table reference. This is not the resolved membership
    map (`Owner := User → Option Role`). Entity rows should store this reference;
    the host resolves it through `OwnerState` before authorization. No new axiom:
    group refs reuse the existing engine-minted `Id`. -/
inductive OwnerRef where
  | world
  | personal (u : User)
  | group (id : Id)

/-- The resolved Leopard-style owner map used by pure authorization rules. -/
abbrev ResolvedOwner : Type := Owner

/-- Server/host owner state: the trusted boundary that resolves stable owner refs
    into current role maps. The kernel does not model the Leopard expansion
    algorithm, SQL tables, or caches; it consumes only this resolved function. -/
structure OwnerState where
  resolve : OwnerRef → ResolvedOwner
  world_resolves : resolve .world = world
  personal_resolves : ∀ u : User, resolve (.personal u) = Owner.ofUser u

/-- A source/flavor ingest batch grouping token. -/
abbrev SourceBatchId     : Type := Id

/-- Which operator produced a derived memory. Reproducibility metadata
    (model id, prompt version, wake depth) stays engine-level row metadata
    (doc 04 §Idempotence — recorded, not axiomatized). -/
opaque OperatorId : Type := String

/-- The F→A input contract: the Fact-schema set the gate row keys on
    (doc 04 §Source-batch lifecycle: "Fact schema set | input contract").
    Opaque: its content is a set of SchemaRefs engine-side; the kernel needs
    only its equality as a gate dimension. -/
opaque InputContract : Type := String

/- `ContentHash` and `EdgeId` retired with the edge id (doc 16 §What This
   Removes). Edge rows have no identity beyond their content, so there is no
   id to represent and no authorship-conditioned id split to constrain; a
   replayed write re-asserts the same primary key instead of minting a
   deduplicable hash. Content-addressability of uploaded artefacts remains an
   engine concern below the payload-opacity boundary. -/

/-- The opaque per-row schema tag every Memory/Goal/CitedObject carries —
    NOT an identity. THE domainless boundary, payload opacity in its strongest
    form: the kernel sees a row is schema-typed but has NO accessor on it at all
    (no resolution, no payload, no capabilities). A flavor's sidecar conforms to
    this schema; the kernel never inspects it. Namespacing, versioning, the flavor
    registry, and admission of registered schemas are engine/build-time concerns,
    not kernel ontology — the universal rules bind every row regardless of which
    flavor produced it. The hidden implementation is a serializable token type;
    no Causa theorem may inspect it. -/
opaque SchemaRef : Type := String

-- ============================================================
-- Lifecycle capability classes (doc 07 §Append-Only)
-- ============================================================

/-- ST-11 — no UPDATE in the cognitive lifecycle. Marker class; an
    instance asserts rows of α are never mutated in place. -/
class AppendOnly (α : Type) : Prop

/-- Never superseded, never updated. Facts, index rows, cited objects,
    citation mappings (ST-2, ST-5..8). -/
class Immutable (α : Type) : Prop

-- ============================================================
-- Source/batch identifiers
-- ============================================================

/- ES-5 — batch-id uniqueness is storage admission, not kernel ontology. The
   current runtime makes `source_batches.id` globally unique and also carries
   the `(source_id, owner, id)` uniqueness witness documented in COVERAGE. Lean
   keeps no source-batch table, so no kernel-observable axiom is required; the
   F→A gate carries its own owner dimension (`FtoaBatchExclusive`,
   Causa.Operators). -/

/- ST-15..17 — vector-store independence is carried by ABSENCE: the kernel
   declares no `Embedding` entity and no `Memory → Embedding` accessor. Entity
   writes never block on embedding; embeddings can be rebuilt or dropped without
   mutating entities; multiple models coexist; re-embedding is a new engine row.
   Embeddings (over F/A/P and Goals) and the fact that edges are never embedded
   as relations are engine concerns — similarity is query-time evidence and never
   authors graph edges (doc 07, grounding U-2). Nothing kernel-side to model. -/

end Causa
