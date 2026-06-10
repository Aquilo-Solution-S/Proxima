/-
Proxima Foundations — Identity

ID types, the Event (EventSource emission — doc 01), and the
append-only storage discipline (doc 07). The storage rule in one
line (doc 07 §Append-Only): INSERT is the normal write path; UPDATE
is not part of the cognitive entity lifecycle; DELETE exists only as
compliance erasure (ST-11, ST-13 — see Foundations.Compliance).

Identity rules (doc 07 §Identity Rules):
  - Fact / Abstraction / Perspective: fresh UUIDv7 MemoryId.
  - Goal: fresh GoalId; supersession writes a new row.
  - Edge: insert-only.
  - Event: deterministic content-hash EventId; duplicate insert is
    silent replay, never a second Fact.
  - Embedding: re-embed writes a new row; the entity row is untouched.
-/

import Foundations.Prelude
import Foundations.Owner

namespace Proxima

-- ============================================================
-- ID slots
-- ============================================================

/-- UUIDv7 — sortable-by-time entity identity (doc 07 §ID Types).
    The kernel keeps UUID structure opaque; "v7" is an engine
    commitment recorded here as commentary. -/
axiom MemoryId : Type
axiom GoalId   : Type
axiom EdgeId   : Type
axiom CitedObjectId      : Type
axiom CitationMappingId  : Type

/-- Deterministic content hash of `(source_id, owner, payload)`
    (doc 01 §Properties of an Event). A dedup key, NOT entity
    identity (ST-22): Fact identity is MemoryId. -/
axiom EventId : Type

axiom SourceId      : Type
axiom SourceBatchId : Type

/-- Opaque (schema_id, schema_version) reference. THE domainless
    boundary: the kernel sees that every Memory/Goal/Event is
    schema-typed, never what the schema contains. Resolution to a
    namespaced SchemaId happens in Foundations.Composition. -/
axiom SchemaRef : Type

-- ============================================================
-- Lifecycle capability classes (doc 07 §Append-Only)
-- ============================================================

/-- ST-11 — no UPDATE in the cognitive lifecycle. Marker class; an
    instance asserts rows of α are never mutated in place. -/
class AppendOnly (α : Type) : Prop

/-- Never superseded, never updated. Facts, Events, Edges, cited
    objects, citation mappings, embeddings (ST-2, ST-5..8, ST-15). -/
class Immutable (α : Type) : Prop

/-- Append-only with supersession: a new row may name the prior head
    it supersedes (ST-3, ST-4; doc 02 §Re-derivation). Supersession
    is logical — current state is a query over append-only rows
    (ST-26). -/
class Supersedable (α : Type) where
  supersedes : α → Option α

-- ============================================================
-- Event — the EventSource emission (doc 01)
-- ============================================================

/-- One typed, cited, deduplicable event crossing the membrane from
    Reality into the agent (doc 01 §The contract). NOTE: "Event" in
    Proxima is the EventSource emission — not WorkingHero's audit
    primitive. Every Fact traces back to exactly one Event; this is
    grounded in Foundations.Memory (`fact_iff_event`). -/
axiom Event : Type
axiom event_id          : Event → EventId
axiom event_source      : Event → SourceId
axiom event_owner       : Event → Owner
axiom event_batch       : Event → SourceBatchId
axiom event_schema      : Event → SchemaRef
axiom event_observed_at : Event → Instant
axiom event_occurred_at : Event → Instant

instance : Immutable Event := ⟨⟩
instance : AppendOnly Event := ⟨⟩

/-- ES-4 / ST-6 — the kernel-visible face of EventId determinism:
    the id is a function of (source, owner, payload), so id equality
    forces source+owner equality. Re-receipt (webhook re-fire, poll
    overlap, manual replay) produces the same id, and the engine
    silently drops the duplicate (ST-23: id collision = replay, not
    error). Payload equality is not expressible here — payloads are
    opaque to the kernel — so this axiom states the projection that
    is. -/
axiom event_id_payload_determined :
  ∀ e1 e2 : Event, event_id e1 = event_id e2 →
    event_source e1 = event_source e2 ∧ event_owner e1 = event_owner e2

/-- ES-5 — source batches group events from one Reality observation;
    the engine validates batch-id uniqueness within `(source_id,
    owner)` and rejects collisions (doc 01 §The contract, Q6). Kernel
    face: a shared batch id implies shared source and owner. -/
axiom batch_unique_within_source_owner :
  ∀ e1 e2 : Event, event_batch e1 = event_batch e2 →
    event_source e1 = event_source e2 ∧ event_owner e1 = event_owner e2

-- ============================================================
-- Embeddings — vector-store independence (doc 07 §Vector Store)
-- ============================================================

/-- ST-15..17 — embeddings reference entity ids but the entity has no
    accessor to its embeddings: there is NO kernel function
    `Memory → Embedding`. That deliberate absence IS the independence
    invariant — entity writes never block on embedding; embeddings can
    be rebuilt or dropped without mutating entities; multiple models
    coexist. Re-embedding writes a new row (`Immutable Embedding`);
    the entity row does not change.

    Embeddings may point at Facts, Abstractions, Perspectives, AND
    Goals (doc 07 §Vector Store) — hence the id-sum target. Edges are
    never embedded as relations: similarity is query-time evidence and
    never authors graph edges (doc 07, grounding U-2). -/
inductive EmbeddingTarget where
  | memory (id : MemoryId)
  | goal   (id : GoalId)

axiom Embedding : Type
axiom embedding_target : Embedding → EmbeddingTarget

instance : Immutable Embedding := ⟨⟩

end Proxima
