/-
Causa — Memory

The F/A/P cognitive graph (doc 02, universe.md). The architectural
commitment, verbatim (universe §Conclusions):

  "this layering is *strict and irreversible*: no operator may
   produce a lower-layer memory from a higher-layer one. That is
   what keeps Facts immutable under Perspective change."

U-1 — the layering is enforced structurally across three files:
  - here: what each kind IS (Fact = memory kind Fact; F/A/P may
    carry optional free text; personality has no materialized slot);
  - Causa.Edges: the directionality rule ℓ(source) ≥ ℓ(target) (E3);
  - Causa.Operators: production shapes (no downward writes).

The Trauma Test (doc 02): Facts are accepted, not revised;
Abstractions and Perspectives are re-derivable; wake-context change
affects future derivations, never existing Facts. Stateful Fact heads are
modeled as `FactEntity` aggregates that point at a current Fact; they do not
replace Fact identity (`MemoryId`).

Supersession and authorship are ROW FIELDS, not connections (doc 16 §The
Model). "The new row replaces the old" is the same thing persisting through
revision, so it is a lineage pointer (`supersedes` / `superseded_by`), written
in the successor's own transaction; "emitted by Perspective P" is known at
write time, so it is a column (`authoring_perspective_id`). Neither is an edge,
which is why the head queries below live here rather than in Causa.Edges.
-/

import Causa.Prelude
import Causa.Owner
import Causa.Identity

namespace Causa

-- ============================================================
-- Kinds and layers (doc 02 §The Layering Principle)
-- ============================================================

inductive MemoryKind where
  | Fact
  | Abstraction
  | Perspective
  deriving DecidableEq, Repr

/-- ℓ(F)=0, ℓ(A)=1, ℓ(P)=2 — doc 02, verbatim. -/
def MemoryKind.layer : MemoryKind → Nat
  | .Fact        => 0
  | .Abstraction => 1
  | .Perspective => 2

-- ============================================================
-- The core entity (doc 02 §The Core Entity)
-- ============================================================

/-- One identity shape for all memories: `memory_id` (UUIDv7),
    per-row `owner`, `schema_id`/`schema_version`, optional free text,
    optional derivation metadata, the lineage and authorship pointers, and
    `created_at` insert time. Text is optional for every kind; flavor sidecars
    may carry additional opaque typed payload. -/
structure Memory where
  id         : MemoryId
  kind       : MemoryKind
  owner      : Owner
  schema     : SchemaRef
  text       : Option Text
  operator   : Option OperatorId
  source_batch : Option SourceBatchId
  input_contract : Option InputContract
  created_at : Instant
  /-- The prior head this row revises. A lineage pointer, not a connection:
      supersession is the same thing persisting through revision, so no index
      row is written for it (doc 16 §The Model). -/
  supersedes : Option MemoryId
  /-- The Perspective that emitted this row, when one did
      (`memories.authoring_perspective_id`). Authorship of a node is a property
      of the node, which is why the edge table has no authorship column and the
      old `EdgeAuthorship` vocabulary has no successor. -/
  authoring_perspective : Option MemoryId
  /-- ME-4 source half, as the runtime states it: the Fact branch of
      `memories_variant_chk` requires `supersedes IS NULL`. A Fact is accepted,
      not revised, so it never supersedes anything. Row-local, so this is a
      structure field rather than a table rule. -/
  fact_never_supersedes : kind = .Fact → supersedes = none

/-- Compatibility accessor for prose/Rust vocabulary. -/
def memory_id : Memory → MemoryId := Memory.id

/-- Compatibility accessor for prose/Rust vocabulary. -/
def memory_kind : Memory → MemoryKind := Memory.kind

/-- Compatibility accessor for prose/Rust vocabulary. -/
def memory_owner : Memory → Owner := Memory.owner

/-- Compatibility accessor for prose/Rust vocabulary. -/
def memory_schema : Memory → SchemaRef := Memory.schema

/-- Compatibility accessor for prose/Rust vocabulary. -/
def memory_text : Memory → Option Text := Memory.text

/-- Compatibility accessor for prose/Rust vocabulary. -/
def memory_operator : Memory → Option OperatorId := Memory.operator

/-- Compatibility accessor for prose/Rust vocabulary. -/
def memory_source_batch : Memory → Option SourceBatchId := Memory.source_batch

/-- Compatibility accessor for prose/Rust vocabulary. -/
def memory_input_contract : Memory → Option InputContract := Memory.input_contract

/-- Compatibility accessor for prose/Rust vocabulary. -/
def memory_created_at : Memory → Instant := Memory.created_at

/-- Compatibility accessor for prose/Rust vocabulary. -/
def memory_supersedes : Memory → Option MemoryId := Memory.supersedes

/-- Compatibility accessor for prose/Rust vocabulary. -/
def memory_authoring_perspective : Memory → Option MemoryId := Memory.authoring_perspective

/-- Regression target: Memory is a structural row shape; core fields are
    reducible projections, not trusted accessors. -/
theorem memory_field_projection
    (id : MemoryId) (kind : MemoryKind) (owner : Owner)
    (schema : SchemaRef) (text : Option Text)
    (operator : Option OperatorId) (source_batch : Option SourceBatchId)
    (input_contract : Option InputContract) (created_at : Instant)
    (supersedes : Option MemoryId) (authoring_perspective : Option MemoryId)
    (fact_never_supersedes : kind = .Fact → supersedes = none) :
    memory_id
      (Memory.mk id kind owner schema text operator source_batch input_contract created_at
        supersedes authoring_perspective fact_never_supersedes) = id := rfl

/-- Derivation metadata is also structural: CN-8 gate dimensions reduce by
    projection on the admitted `Memory` row. -/
theorem memory_derivation_field_projection
    (id : MemoryId) (kind : MemoryKind) (owner : Owner)
    (schema : SchemaRef) (text : Option Text)
    (operator : Option OperatorId) (source_batch : Option SourceBatchId)
    (input_contract : Option InputContract) (created_at : Instant)
    (supersedes : Option MemoryId) (authoring_perspective : Option MemoryId)
    (fact_never_supersedes : kind = .Fact → supersedes = none) :
    memory_operator
        (Memory.mk id kind owner schema text operator source_batch input_contract created_at
          supersedes authoring_perspective fact_never_supersedes) =
          operator ∧
      memory_source_batch
        (Memory.mk id kind owner schema text operator source_batch input_contract created_at
          supersedes authoring_perspective fact_never_supersedes) =
          source_batch ∧
      memory_input_contract
        (Memory.mk id kind owner schema text operator source_batch input_contract created_at
          supersedes authoring_perspective fact_never_supersedes) =
          input_contract := by
  constructor
  · rfl
  constructor
  · rfl
  · rfl

/-- Lineage and authorship are structural too: both are ordinary field
    projections on the row, never a connection or an accessor on an index. -/
theorem memory_lineage_field_projection
    (id : MemoryId) (kind : MemoryKind) (owner : Owner)
    (schema : SchemaRef) (text : Option Text)
    (operator : Option OperatorId) (source_batch : Option SourceBatchId)
    (input_contract : Option InputContract) (created_at : Instant)
    (supersedes : Option MemoryId) (authoring_perspective : Option MemoryId)
    (fact_never_supersedes : kind = .Fact → supersedes = none) :
    memory_supersedes
        (Memory.mk id kind owner schema text operator source_batch input_contract created_at
          supersedes authoring_perspective fact_never_supersedes) = supersedes ∧
      memory_authoring_perspective
        (Memory.mk id kind owner schema text operator source_batch input_contract created_at
          supersedes authoring_perspective fact_never_supersedes) = authoring_perspective := by
  constructor
  · rfl
  · rfl

/-- ME-id — memory_id uniqueness is a table/store invariant, not a
    global property of raw `Memory` values. A set with duplicate ids is
    invalid even though the structure type itself remains constructible. -/
def MemoryIdUnique (memories : Set Memory) : Prop :=
  ∀ m1 m2 : Memory,
    m1 ∈ memories →
    m2 ∈ memories →
    memory_id m1 = memory_id m2 →
    m1 = m2

instance : AppendOnly Memory := ⟨⟩

-- ============================================================
-- Facts (doc 02 §The Layering Principle)
-- ============================================================

/-- A Fact is not a second kernel entity. It is exactly a `Memory`
    whose kind is `.Fact`. Source/flavor ingest vocabulary lives
    outside the core kernel and materializes typed Facts. -/
def Fact : Type := { m : Memory // memory_kind m = .Fact }

/-- Projection from the Fact subtype back to the memory row. -/
def Fact.memory (f : Fact) : Memory := f.val

/-- ME-1 — Facts are structurally memories with kind `.Fact`; no
    source-event axiom is required. -/
theorem fact_memory_kind (f : Fact) : memory_kind f.memory = .Fact := f.property

-- ============================================================
-- Supersession — a lineage pointer, never a connection
-- (doc 02 §Re-derivation and Supersession; doc 16 §The Model)
-- ============================================================

/-- ME-4, source half — a Fact never supersedes. THEOREM by projection from
    the row's own constraint; no table hypothesis is needed, because the
    runtime CHECK (`memories_variant_chk`, Fact branch) is row-local. -/
theorem fact_supersedes_nothing (f : Fact) : memory_supersedes f.memory = none :=
  f.val.fact_never_supersedes f.property

/-- ST-26 — memory supersession is the successor's lineage pointer resolved
    inside the admitted Memory table. There is no supersession edge to look
    for: the successor's row names the head it replaced, in its own write. -/
def memorySupersedes (memories : Set Memory) (new old : Memory) : Prop :=
  new ∈ memories ∧ old ∈ memories ∧ memory_supersedes new = some (memory_id old)

/-- Supersession ids resolve to rows in the actual Memory table (the Goal-side
    analogue is `GoalSupersessionResolved`). -/
def MemorySupersessionResolved (memories : Set Memory) : Prop :=
  ∀ (m : Memory) (prior : MemoryId),
    m ∈ memories →
    memory_supersedes m = some prior →
    ∃ p : Memory, p ∈ memories ∧ memory_id p = prior

/-- ME-5a + ME-5b — table-scoped supersession validity: successor and prior
    head share Owner and kind. This is exactly what the write path checks
    (`validate_supersedes_in_owner`: the prior row must be in the same owner
    and carry the same `kind`). -/
structure MemorySupersessionValid (memories : Set Memory) : Prop where
  sameOwner : ∀ new old : Memory,
    memorySupersedes memories new old → memory_owner new = memory_owner old
  sameKind : ∀ new old : Memory,
    memorySupersedes memories new old → memory_kind new = memory_kind old

/-- ME-5b in its original shape — projection theorem. -/
theorem memory_supersession_same_owner :
    ∀ (memories : Set Memory), MemorySupersessionValid memories →
      ∀ new old : Memory, memorySupersedes memories new old →
        memory_owner new = memory_owner old := by
  intro memories hvalid new old h
  exact hvalid.sameOwner new old h

/-- ME-5a in its original shape — projection theorem. -/
theorem memory_supersession_same_kind :
    ∀ (memories : Set Memory), MemorySupersessionValid memories →
      ∀ new old : Memory, memorySupersedes memories new old →
        memory_kind new = memory_kind old := by
  intro memories hvalid new old h
  exact hvalid.sameKind new old h

/-- ME-4 / SR-14/44 — "Facts never supersede and are never superseded"
    (doc 02, verbatim). THEOREM in both halves: the source half from the row's
    own `fact_never_supersedes`, the target half from same-kind validity
    feeding the same row constraint. -/
theorem facts_never_supersede :
    ∀ (memories : Set Memory), MemorySupersessionValid memories →
      ∀ new old : Memory, memorySupersedes memories new old →
        memory_kind new ≠ .Fact ∧ memory_kind old ≠ .Fact := by
  intro memories hvalid new old h
  have hkind := hvalid.sameKind new old h
  have hnew : memory_kind new ≠ .Fact := by
    intro hf
    have hnone : memory_supersedes new = none := new.fact_never_supersedes hf
    have hsome : memory_supersedes new = some (memory_id old) := h.2.2
    rw [hnone] at hsome
    exact (nomatch hsome)
  refine ⟨hnew, ?_⟩
  intro hf
  exact hnew (hkind.trans hf)

/-- GO-2b analogue — at most one successor row may name the same prior head. -/
def MemorySuccessorUnique (memories : Set Memory) : Prop :=
  ∀ (m1 m2 : Memory) (prior : MemoryId),
    m1 ∈ memories →
    m2 ∈ memories →
    memory_supersedes m1 = some prior →
    memory_supersedes m2 = some prior →
    m1 = m2

/-- A Memory lifecycle head in the actual admitted table: the row is present
    and no admitted row names it as the head it replaced. -/
def memoryIsHead (memories : Set Memory) (m : Memory) : Prop :=
  m ∈ memories ∧
  ¬ ∃ m' : Memory, m' ∈ memories ∧ memory_supersedes m' = some (memory_id m)

/-- Generic current Memory-head query. -/
def memoryHeads (memories : Set Memory) : Set Memory :=
  fun m => memoryIsHead memories m

/-- Generic current Perspective-head query. Downstream apps add their own
    schema/payload filters; the kernel supplies only the F/A/P head shape. -/
def perspectiveHeads (memories : Set Memory) : Set Memory :=
  fun m => memory_kind m = .Perspective ∧ memoryIsHead memories m

/-- Superseded rows are not Memory lifecycle heads. -/
theorem memory_superseded_not_head :
    ∀ (memories : Set Memory) (new old : Memory),
      memorySupersedes memories new old → ¬ memoryIsHead memories old := by
  intro memories new old hsup hhead
  exact hhead.2 ⟨new, hsup.1, hsup.2.2⟩

/-- Projection: a Perspective head is a Perspective. -/
theorem perspective_head_is_perspective :
    ∀ (memories : Set Memory) (m : Memory),
      m ∈ perspectiveHeads memories → memory_kind m = .Perspective := by
  intro memories m h
  exact h.1

/-- Projection: a Perspective head is also a Memory head. -/
theorem perspective_head_is_memory_head :
    ∀ (memories : Set Memory) (m : Memory),
      m ∈ perspectiveHeads memories → memoryIsHead memories m := by
  intro memories m h
  exact h.2

-- ============================================================
-- Stateful Fact heads (doc 03 §Stateful Fact schemas)
-- ============================================================

/-- Opaque-to-the-kernel sidecar-declared natural key for a stateful Fact
    schema. Lean represents it as uninterpreted text; the kernel never parses
    it and never derives entity identity from it. -/
abbrev NaturalKey : Type := Text

/-- Stable reference aggregate for a stateful Fact. It is not a new semantic
    memory kind: it carries no payload/text/citation/provenance, only a fresh
    handle and the current immutable Fact version selected by the sidecar
    natural-key head query. -/
structure FactEntity where
  id          : FactEntityId
  owner       : Owner
  schema      : SchemaRef
  natural_key : NaturalKey
  current     : Fact
  current_owner : memory_owner current.memory = owner
  current_schema : memory_schema current.memory = schema

/-- Compatibility accessor for prose/Rust vocabulary. -/
def fact_entity_id : FactEntity → FactEntityId := FactEntity.id

/-- Compatibility accessor for prose/Rust vocabulary. -/
def fact_entity_owner : FactEntity → Owner := FactEntity.owner

/-- Compatibility accessor for prose/Rust vocabulary. -/
def fact_entity_schema : FactEntity → SchemaRef := FactEntity.schema

/-- Compatibility accessor for prose/Rust vocabulary. -/
def fact_entity_natural_key : FactEntity → NaturalKey := FactEntity.natural_key

/-- Compatibility accessor for prose/Rust vocabulary. -/
def fact_entity_current : FactEntity → Fact := FactEntity.current

/-- Fact-entity id uniqueness is a table/store invariant, not a content-derived
    property of the natural key. -/
def FactEntityIdUnique (entities : Set FactEntity) : Prop :=
  ∀ e1 e2 : FactEntity,
    e1 ∈ entities →
    e2 ∈ entities →
    fact_entity_id e1 = fact_entity_id e2 →
    e1 = e2

/-- Stateful Fact entity uniqueness is by the declared natural-key tuple within
    `(owner, schema)`. The id remains a fresh surrogate; the natural key is the
    table uniqueness guard, not a content-addressable identity. -/
def FactEntityNaturalKeyUnique (entities : Set FactEntity) : Prop :=
  ∀ e1 e2 : FactEntity,
    e1 ∈ entities →
    e2 ∈ entities →
    fact_entity_owner e1 = fact_entity_owner e2 →
    fact_entity_schema e1 = fact_entity_schema e2 →
    fact_entity_natural_key e1 = fact_entity_natural_key e2 →
    e1 = e2

/-- A FactEntity's current version is always a Fact. -/
theorem factEntityCurrentIsFact :
    ∀ e : FactEntity, memory_kind e.current.memory = .Fact := by
  intro e
  exact fact_memory_kind e.current

/-- A FactEntity current head is owner-aligned with the aggregate row. -/
theorem factEntityCurrentOwner :
    ∀ e : FactEntity, memory_owner e.current.memory = fact_entity_owner e := by
  intro e
  exact e.current_owner

/-- A FactEntity current head is schema-aligned with the aggregate row. -/
theorem factEntityCurrentSchema :
    ∀ e : FactEntity, memory_schema e.current.memory = fact_entity_schema e := by
  intro e
  exact e.current_schema

-- ============================================================
-- Personality absence (doc 02 §Personality, D4)
-- ============================================================

/- Personality is not a kernel entity, row, instance, or decider slot.
   It emerges from Perspective/context supplied to a wake call. The
   kernel therefore has no `PersonalityInstance`, no `personality_owner`,
   no `memory_authoring_personality`, and no read-scope matrix over
   materialized personalities. Wake-entry/context semantics are deferred
   to the source/fact-based wake model. -/

end Causa
