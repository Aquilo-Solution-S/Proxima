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
  - Causa.Edges: the directionality rule ℓ(source) ≥ ℓ(target)
    and the class-legality matrix;
  - Causa.Operators: production shapes (no downward writes).

The Trauma Test (doc 02): Facts are accepted, not revised;
Abstractions and Perspectives are re-derivable; wake-context change
affects future derivations, never existing Facts.
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
    and `created_at` insert time. Text is optional for every kind;
    flavor sidecars may carry additional opaque typed payload. -/
structure Memory where
  id         : MemoryId
  kind       : MemoryKind
  owner      : Owner
  schema     : SchemaRef
  text       : Option Text
  created_at : Instant

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
def memory_created_at : Memory → Instant := Memory.created_at

/-- Regression target: Memory is a structural row shape, not only an
    opaque type plus opaque accessor axioms. -/
theorem memory_field_projection
    (id : MemoryId) (kind : MemoryKind) (owner : Owner)
    (schema : SchemaRef) (text : Option Text) (created_at : Instant) :
    memory_id (Memory.mk id kind owner schema text created_at) = id := rfl

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
-- Personality absence (doc 02 §Personality, D4)
-- ============================================================

/- Personality is not a kernel entity, row, instance, or decider slot.
   It emerges from Perspective/context supplied to a wake call. The
   kernel therefore has no `PersonalityInstance`, no `personality_owner`,
   no `memory_authoring_personality`, and no read-scope matrix over
   materialized personalities. Wake-entry/context semantics are deferred
   to the source/fact-based wake model. -/

end Causa
