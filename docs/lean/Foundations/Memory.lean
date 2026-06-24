/-
Proxima Foundations — Memory

The F/A/P cognitive graph (doc 02, universe.md). The architectural
commitment, verbatim (universe §Conclusions):

  "this layering is *strict and irreversible*: no operator may
   produce a lower-layer memory from a higher-layer one. That is
   what keeps Facts immutable under Perspective change."

U-1 — the layering is enforced structurally across three files:
  - here: what each kind IS (Fact ↔ sourced from an Event; A/P ↔
    authored text + personality);
  - Foundations.Edges: the directionality rule ℓ(source) ≥ ℓ(target)
    and the class-legality matrix;
  - Foundations.Operators: production shapes (no downward writes).

The Trauma Test (doc 02): Facts are accepted, not revised;
Abstractions and Perspectives are re-derivable; personality change
affects future derivations, never existing Facts.
-/

import Foundations.Prelude
import Foundations.Owner
import Foundations.Identity

namespace Proxima

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
    per-row `owner`, `schema_id`/`schema_version` present for every
    memory, `created_at` insert time. -/
axiom Memory : Type
axiom memory_id         : Memory → MemoryId
axiom memory_kind       : Memory → MemoryKind
axiom memory_owner      : Memory → Owner
axiom memory_schema     : Memory → SchemaRef
axiom memory_created_at : Memory → Instant

/-- ME-id — memory_id is identity: two memories with the same id are
    the same memory. -/
axiom memory_id_injective :
  ∀ m1 m2 : Memory, memory_id m1 = memory_id m2 → m1 = m2

instance : AppendOnly Memory := ⟨⟩

-- ============================================================
-- Facts trace to Events (doc 01: "Every Fact in the system traces
-- back to an Event Source. No exceptions.")
-- ============================================================

axiom memory_source_event : Memory → Option Event

/-- ME-1 — a memory is a Fact IFF it carries a source event. The ←
    direction makes Facts the ONLY observation kind; the → direction
    is doc 01's "no exceptions". -/
axiom fact_iff_event :
  ∀ m : Memory, memory_kind m = .Fact ↔ (memory_source_event m).isSome

/-- ME-2 — a Fact lives in its event's Owner scope (doc 01: the
    event's `owner` is "whose Reality slice"; the Fact inherits it). -/
axiom fact_event_owner :
  ∀ (m : Memory) (e : Event),
    memory_source_event m = some e → memory_owner m = event_owner e

-- ============================================================
-- Authored text (doc 02 §The Core Entity)
-- ============================================================

axiom memory_text : Memory → Option Text

/-- ME-3 — text iff derived. Facts have no stored text (SR-11: render
    from payload on demand); Abstractions and Perspectives ALWAYS
    carry immutable operator-authored text (SR-16: narrative,
    rationale, hedging live there — "A/P are always typed and always
    carry immutable text", doc 02 §What's Settled). -/
axiom text_iff_derived :
  ∀ m : Memory, (memory_text m).isSome ↔ memory_kind m ≠ .Fact

-- ============================================================
-- Supersession (doc 02 §Re-derivation and Supersession)
-- ============================================================

/-- The supersession pointer: `new_entity --core/supersedes-->
    old_entity`, append-only — modeled as an accessor on the NEW row.
    Engine-side this IS the `core/supersedes` edge; the bridge axiom
    `supersession_pointer_is_edge` (Foundations.Edges) pins that
    identification, and ME-4 (Facts never supersede), ME-5a (same
    kind), ME-5b (same owner) are PROVED there from the edge matrix
    and edge scope — minimization pass, 2026-06-11. -/
axiom memory_supersedes : Memory → Option Memory

noncomputable instance : Supersedable Memory := ⟨memory_supersedes⟩

-- ============================================================
-- Personality (doc 02 §Personality)
-- ============================================================

/-- Runtime decider identity. Type-level behavior comes from the
    registered flavor; the kernel commits to instance identity, Owner
    scope, and authorship of derived memories. Multiple instances may
    be active for one Owner — same inputs under different instances
    produce parallel lineages. -/
axiom PersonalityInstance : Type
axiom personality_owner : PersonalityInstance → Owner

/-- Authoring personality of a derived memory (reproducibility
    metadata lives inline on the row — doc 02 §The Core Entity).
    `none` for Facts (Foundations.Operators, `facts_only_from_sources`)
    and for non-personality-bound writes. -/
axiom memory_authoring_personality : Memory → Option PersonalityInstance

/-- ME-6 — an authoring personality shares the memory's Owner
    (wake execution is per Owner — doc 04 §Execution model). -/
axiom authoring_personality_owner :
  ∀ (m : Memory) (p : PersonalityInstance),
    memory_authoring_personality m = some p →
    personality_owner p = memory_owner m

-- ============================================================
-- Read-scope matrix (doc 02 §Read-scope Matrix)
-- ============================================================

/-- Per-Owner boolean adjacency over personality instances:
    `read_scope o self other` ⇔ self may read other's A/P/Goals.

    ME-7 — Facts sit BELOW the matrix: every personality sees every
    Fact in the Owner; only A/P/Goal retrieval is gated. Encoded by
    `personality_may_read` below, which is unconditional for Facts.

    ME-8 — asymmetry is valid: no symmetry axiom exists, and that
    absence is the spec. Changing the matrix affects future reads
    only; existing memories remain (append-only). The matrix governs
    direct retrieval only — transitive influence flows through
    authored memories and provenance edges. -/
axiom read_scope : Owner → PersonalityInstance → PersonalityInstance → Prop

/-- Identity diagonal: M[p][p] = 1 (doc 02, verbatim) — scoped to the
    Owner's own matrix: the doc defines M per Owner over THAT Owner's
    instances; asserting the diagonal for foreign (o, p) pairs would
    exceed the doc (minimization pass, weaken-overstrong). -/
axiom read_scope_diagonal :
  ∀ (o : Owner) (p : PersonalityInstance),
    personality_owner p = o → read_scope o p p

/-- ME-7 — what personality `p` may retrieve, within one Owner:
    Facts unconditionally; A/P gated by the matrix against the
    authoring instance (un-authored derived rows are substrate-
    visible). A definition: the kernel fixes the rule's content. -/
def personality_may_read (p : PersonalityInstance) (m : Memory) : Prop :=
  personality_owner p = memory_owner m ∧
  (memory_kind m = .Fact ∨
    match memory_authoring_personality m with
    | some author => read_scope (memory_owner m) p author
    | none        => True)

end Proxima
