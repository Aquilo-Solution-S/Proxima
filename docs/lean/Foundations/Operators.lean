/-
Proxima Foundations — Operators

The production rules (doc 02 §The Layering Principle, doc 04):

  F→A   : 2^F × Π → A      facts become one typed Abstraction
  A→P   : 2^A × Π → P      abstractions become one typed Perspective
  A→Goal: 2^A × Π → Goal   goals derived from Abstraction evidence
  frame : P × A_cross → Edge

Π = the active personality instance. In spec-mode Lean, operators are
not modeled as functions — entities plus write-shape obligations
carry the same content: what each authorship class may produce, and
what every derived memory must possess.

CN-5 — no downward writes (A→F, P→A, P→F): the kernel face is that
Facts are NEVER operator/personality products. Combined with ME-1
(Fact ⟷ source event), the only path into F is the EventSource
membrane. "Dreaming" needs no axioms: dream outputs are ordinary
typed writes under these same rules (doc 02 §Wake/Dream/Write — no
Dream entity, no Dream relation class, no Core dream pipeline; the
ABSENCE of dream primitives here is deliberate).

CN-9 (atomic invocation) and the wake-dispatcher loop are
storage/runtime contracts, not kernel axioms — same stance WH takes
on event/projection atomicity. Recorded as exclusions in COVERAGE.md.
-/

import Foundations.Prelude
import Foundations.Owner
import Foundations.Identity
import Foundations.Memory
import Foundations.Goals
import Foundations.Edges

namespace Proxima

-- ============================================================
-- CN-5 — no downward writes
-- ============================================================

/-- Facts are observations, never derivations: no Fact carries an
    authoring personality (doc 01 §What the Event Source must not do;
    doc 02 forbidden writes A→F, P→A, P→F). With ME-1, Facts enter
    only through the membrane. -/
axiom facts_only_from_sources :
  ∀ m : Memory, memory_kind m = .Fact →
    memory_authoring_personality m = none

-- ============================================================
-- Operator edge shapes (doc 02 §Provenance, §Edge Scope authorship)
-- ============================================================

/-- CN-1 — F→A writes `Abstraction → Fact*` provenance edges:
    an OperatorFtoA-authored edge is Provenance-class from an
    Abstraction to a Fact. -/
axiom ftoa_edge_shape :
  ∀ e : Edge, edge_authorship e = .OperatorFtoA →
    relation_class (edge_relation e) = .Provenance ∧
    (∃ ms : Memory, edge_source e = .memory ms ∧ memory_kind ms = .Abstraction) ∧
    (∃ mt : Memory, edge_target e = .memory mt ∧ memory_kind mt = .Fact)

/-- CN-2 — A→P writes `Perspective → Abstraction*` provenance edges. -/
axiom atop_edge_shape :
  ∀ e : Edge, edge_authorship e = .OperatorAtoP →
    relation_class (edge_relation e) = .Provenance ∧
    (∃ ms : Memory, edge_source e = .memory ms ∧ memory_kind ms = .Perspective) ∧
    (∃ mt : Memory, edge_target e = .memory mt ∧ memory_kind mt = .Abstraction)

/-- CN-3 — A→Goal evidence: an OperatorAtoGoal-authored edge is
    Structural-class from a Goal to its Fact/Abstraction evidence
    (doc 02 §Relation Registry: `proxima-goal/motivated-by` |
    `Structural` | Goal → Fact / Abstraction; never to a
    Perspective). -/
axiom atogoal_edge_shape :
  ∀ e : Edge, edge_authorship e = .OperatorAtoGoal →
    relation_class (edge_relation e) = .Structural ∧
    (∃ g : Goal, edge_source e = .goal g) ∧
    (∃ mt : Memory, edge_target e = .memory mt ∧ memory_kind mt ≠ .Perspective)

/-- CN-4 — frame: PerspectiveLink-authored edges are P-authored
    causal/interpretive framing — Causal or Interpretive class, with
    a Perspective source ("Perspective is the locus of causal claims",
    universe §Philosophical commitments). Facts stay unchanged: a
    frame is an edge write, never a Fact write. -/
axiom perspective_link_shape :
  ∀ e : Edge, edge_authorship e = .PerspectiveLink →
    (relation_class (edge_relation e) = .Causal ∨
     relation_class (edge_relation e) = .Interpretive) ∧
    (∃ ms : Memory, edge_source e = .memory ms ∧ memory_kind ms = .Perspective)

-- ============================================================
-- CN-6 — derived memories have provenance
-- ============================================================

/-- Every Abstraction has at least one F→A provenance edge down to a
    Fact; every Perspective at least one provenance edge down to an
    Abstraction (doc 02 §Provenance; bibliographic provenance for A/P
    is the transitive closure to Facts — CI-3). Cross-domain synthesis
    (CN-7) is the same shape: `Abstraction_cross → Fact*` with
    provenance to EVERY input Fact — the typed Abstraction is the only
    cross-domain join object. -/
axiom abstraction_has_provenance :
  ∀ m : Memory, memory_kind m = .Abstraction →
    ∃ e : Edge, edge_source e = .memory m ∧
      relation_class (edge_relation e) = .Provenance ∧
      (∃ mt : Memory, edge_target e = .memory mt ∧ memory_kind mt = .Fact)

axiom perspective_has_provenance :
  ∀ m : Memory, memory_kind m = .Perspective →
    ∃ e : Edge, edge_source e = .memory m ∧
      relation_class (edge_relation e) = .Provenance ∧
      (∃ mt : Memory, edge_target e = .memory mt ∧ memory_kind mt = .Abstraction)

-- ============================================================
-- CN-8 — F→A source-batch gate (doc 04 §Source-batch lifecycle,
-- §Phase 2 F→A exclusivity)
-- ============================================================

/-- Which operator produced a derived memory, and from which source
    batch. Opaque operator identity; reproducibility metadata
    (model id, prompt version, wake depth) stays engine-level row
    metadata (doc 04 §Idempotence — recorded, not axiomatized). -/
axiom OperatorId : Type
axiom memory_operator : Memory → Option OperatorId
axiom memory_source_batch : Memory → Option SourceBatchId

/-- The F→A input contract: the Fact-schema set the gate row keys on
    (doc 04 §Source-batch lifecycle: "Fact schema set | input
    contract"). Opaque — its content is a set of SchemaRefs
    engine-side; the kernel needs only its identity as a gate
    dimension. -/
axiom InputContract : Type
axiom memory_input_contract : Memory → Option InputContract

/-- F→A exclusivity, per doc 04 §Phase 2: "Exclusive per (input
    contract, operator id, output Abstraction schema)" within one
    source batch. Kernel face: two Abstractions agreeing on ALL FOUR
    gate dimensions are the same memory. The same operator may emit a
    new row when the input contract OR output schema differs;
    multiple operators on one batch stay legal. -/
axiom ftoa_batch_exclusive :
  ∀ m1 m2 : Memory,
    memory_kind m1 = .Abstraction → memory_kind m2 = .Abstraction →
    memory_source_batch m1 ≠ none →
    memory_source_batch m1 = memory_source_batch m2 →
    memory_input_contract m1 = memory_input_contract m2 →
    memory_operator m1 = memory_operator m2 →
    memory_schema m1 = memory_schema m2 →
    m1 = m2

end Proxima
