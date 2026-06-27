/-
Causa — Operators

The production rules (doc 02 §The Layering Principle, doc 04):

  F→A   : 2^F × wake context → A      facts become one typed Abstraction
  A→P   : 2^A × wake context → P      abstractions become one typed Perspective
  A→Goal: 2^A × wake context → Goal   goals derived from Abstraction evidence
  frame : P × A_cross → Edge

Wake context is not a materialized PersonalityInstance. In spec-mode
Lean, operators are not modeled as functions — entities plus
write-shape obligations carry the same content: what each authorship
class may produce, and what every derived memory must possess.

Minimized trusted core (2026-06-11): the four per-authorship shape
axioms (CN-1..4) are ONE axiom over the `operatorEdgeShape` def; the
target-kind conjuncts of CN-1/CN-2 and of the provenance obligation
are PROVED from the edge matrix (Provenance pins the target kind
uniquely given the source kind).

CN-5 — no downward writes (A→F, P→A, P→F): the kernel face is the
operator edge shape plus the class-legality matrix. Source/flavor
ingest may materialize typed Facts, but the core kernel does not model
a separate Event entity or materialized PersonalityInstance. "Dreaming" needs no axioms: dream outputs are ordinary
typed writes under these same rules (doc 02 §Wake/Dream/Write — no
Dream entity, no Dream relation class, no Core dream pipeline; the
ABSENCE of dream primitives here is deliberate).

CN-9 (atomic invocation) and the wake-dispatcher loop are
storage/runtime contracts, not kernel axioms — same stance WH takes
on event/projection atomicity. Recorded as exclusions in COVERAGE.md.
-/

import Causa.Prelude
import Causa.Owner
import Causa.Identity
import Causa.Memory
import Causa.Goals
import Causa.Edges

namespace Causa

-- ============================================================
-- Operator edge shapes (doc 02 §Provenance, §Edge Scope authorship;
-- doc 02 §Relation Registry for motivated-by's Structural class)
-- ============================================================

/-- What each authorship class is permitted to write — CN-1..CN-4 as
    one def over the closed `EdgeAuthorship` vocabulary (the same
    move `legalClasses` makes for kinds). Non-operator authorships
    (source-ingest, Engine, User, ExternalAgent) carry no extra shape
    here: their legality is the matrix + masks. Target kind for A→P
    remains matrix-forced; F→A states its Fact target directly because
    A→A provenance is legal. -/
def operatorEdgeShape : EdgeAuthorship → Edge → Prop
  | .OperatorFtoA, e =>
      relation_class (edge_relation e) = .Provenance ∧
      (∃ ms : Memory, edge_source e = .memory ms ∧ memory_kind ms = .Abstraction) ∧
      (∃ mt : Memory, edge_target e = .memory mt ∧ memory_kind mt = .Fact)
  | .OperatorAtoA, e =>
      relation_class (edge_relation e) = .Provenance ∧
      (∃ ms : Memory, edge_source e = .memory ms ∧ memory_kind ms = .Abstraction) ∧
      (∃ mt : Memory, edge_target e = .memory mt ∧ memory_kind mt = .Abstraction)
  | .OperatorAtoP, e =>
      relation_class (edge_relation e) = .Provenance ∧
      (∃ ms : Memory, edge_source e = .memory ms ∧ memory_kind ms = .Perspective) ∧
      (∃ mt : Memory, edge_target e = .memory mt)
  | .OperatorAtoGoal, e =>
      relation_class (edge_relation e) = .Structural ∧
      (∃ g : Goal, edge_source e = .goal g) ∧
      (∃ mt : Memory, edge_target e = .memory mt ∧ memory_kind mt ≠ .Perspective)
  | .PerspectiveLink, e =>
      (relation_class (edge_relation e) = .Causal ∨
       relation_class (edge_relation e) = .Interpretive) ∧
      (∃ ms : Memory, edge_source e = .memory ms ∧ memory_kind ms = .Perspective)
  | .PerspectiveGoalLink, e =>
      relation_class (edge_relation e) = .Causal ∧
      (∃ g : Goal, edge_source e = .goal g) ∧
      (∃ mt : Memory, edge_target e = .memory mt ∧ memory_kind mt = .Fact)
  | _, _ => True

/-- CN-1..CN-4 — every edge satisfies its authorship's shape. -/
axiom operator_edges_shaped :
  ∀ e : Edge, operatorEdgeShape (edge_authorship e) e

/-- CN-5 — operator memory outputs are never Facts. The output side of
    F→A/A→A/A→P operator edges is the source endpoint by Proxima's
    provenance direction convention (`new -> inputs`). -/
theorem operator_memory_output_not_fact :
    ∀ (e : Edge) (m : Memory),
      (edge_authorship e = .OperatorFtoA ∨
       edge_authorship e = .OperatorAtoA ∨
       edge_authorship e = .OperatorAtoP) →
      edge_source e = .memory m →
      memory_kind m ≠ .Fact := by
  intro e m ha hsout
  rcases ha with hfa | ha
  · have h := operator_edges_shaped e
    rw [hfa] at h
    rcases h with ⟨_, ⟨ms, hsrc, hkind⟩, _⟩
    rw [hsout] at hsrc
    injection hsrc with heq
    rw [heq, hkind]
    intro hfalse
    exact (nomatch hfalse)
  · rcases ha with haa | hap
    · have h := operator_edges_shaped e
      rw [haa] at h
      rcases h with ⟨_, ⟨ms, hsrc, hkind⟩, _⟩
      rw [hsout] at hsrc
      injection hsrc with heq
      rw [heq, hkind]
      intro hfalse
      exact (nomatch hfalse)
    · have h := operator_edges_shaped e
      rw [hap] at h
      rcases h with ⟨_, ⟨ms, hsrc, hkind⟩, _⟩
      rw [hsout] at hsrc
      injection hsrc with heq
      rw [heq, hkind]
      intro hfalse
      exact (nomatch hfalse)

/-- Helper: a Provenance-class memory→memory edge with a known source
    kind has its target kind pinned by the matrix. -/
theorem provenance_pins_target :
    ∀ (e : Edge) (ms mt : Memory),
      edge_source e = .memory ms → edge_target e = .memory mt →
      relation_class (edge_relation e) = .Provenance →
      (memory_kind ms = .Abstraction →
        memory_kind mt = .Fact ∨ memory_kind mt = .Abstraction) ∧
      (memory_kind ms = .Perspective → memory_kind mt = .Abstraction) := by
  intro e ms mt hs ht hc
  have hleg := edge_class_legal e ms mt hs ht
  rw [hc] at hleg
  constructor
  · intro hk
    rw [hk] at hleg
    revert hleg
    cases memory_kind mt <;> intro hleg <;>
      first
        | exact Or.inl rfl
        | exact Or.inr rfl
        | exact hleg.elim
        | (rcases hleg with h' | h' <;> first | exact (nomatch h') | exact (nomatch h'))
  · intro hk
    rw [hk] at hleg
    revert hleg
    cases memory_kind mt <;> intro hleg <;>
      first
        | rfl
        | exact hleg.elim
        | (rcases hleg with h' | h'
           · exact (nomatch h')
           · rcases h' with h'' | h''
             · exact (nomatch h'')
             · rcases h'' with h3 | h3 <;> exact (nomatch h3))

/-- CN-1 in full — F→A writes `Abstraction → Fact` provenance edges.
    THEOREM: the target kind is matrix-forced. -/
theorem ftoa_edge_shape :
    ∀ e : Edge, edge_authorship e = .OperatorFtoA →
      relation_class (edge_relation e) = .Provenance ∧
      (∃ ms : Memory, edge_source e = .memory ms ∧ memory_kind ms = .Abstraction) ∧
      (∃ mt : Memory, edge_target e = .memory mt ∧ memory_kind mt = .Fact) := by
  intro e ha
  have h := operator_edges_shaped e
  rw [ha] at h
  exact h

/-- CN-2 in full — A→P writes `Perspective → Abstraction` provenance
    edges. THEOREM: the target kind is matrix-forced. -/
theorem atop_edge_shape :
    ∀ e : Edge, edge_authorship e = .OperatorAtoP →
      relation_class (edge_relation e) = .Provenance ∧
      (∃ ms : Memory, edge_source e = .memory ms ∧ memory_kind ms = .Perspective) ∧
      (∃ mt : Memory, edge_target e = .memory mt ∧ memory_kind mt = .Abstraction) := by
  intro e ha
  have h := operator_edges_shaped e
  rw [ha] at h
  obtain ⟨hc, ⟨ms, hs, hk⟩, ⟨mt, ht⟩⟩ := h
  exact ⟨hc, ⟨ms, hs, hk⟩,
    ⟨mt, ht, (provenance_pins_target e ms mt hs ht hc).2 hk⟩⟩

-- ============================================================
-- CN-6 — derived memories have provenance (doc 02 §Provenance)
-- ============================================================

/-- Every derived memory (Abstraction or Perspective) has at least
    one downward Provenance edge (doc 02 §Provenance: "F→A writes
    Abstraction → Fact* provenance edges; A→P writes Perspective →
    Abstraction*"). Merged CN-6 (minimization pass); the per-kind
    target kinds are matrix-forced (theorems below). Cross-domain
    synthesis (CN-7) is the same shape with provenance to EVERY input
    Fact — the typed Abstraction is the only cross-domain join
    object. Bibliographic provenance for A/P is the transitive
    closure to Facts (CI-3). -/
axiom derived_has_provenance :
  ∀ m : Memory, memory_kind m ≠ .Fact →
    ∃ e : Edge, edge_source e = .memory m ∧
      relation_class (edge_relation e) = .Provenance ∧
      (∃ mt : Memory, edge_target e = .memory mt)

/-- CN-6a in original shape — every Abstraction has F-provenance. -/
theorem abstraction_has_provenance :
    ∀ m : Memory, memory_kind m = .Abstraction →
      ∃ e : Edge, edge_source e = .memory m ∧
        relation_class (edge_relation e) = .Provenance ∧
        (∃ mt : Memory, edge_target e = .memory mt ∧
          (memory_kind mt = .Fact ∨ memory_kind mt = .Abstraction)) := by
  intro m hk
  have hne : memory_kind m ≠ .Fact := by rw [hk]; intro h; exact (nomatch h)
  obtain ⟨e, hs, hc, ⟨mt, ht⟩⟩ := derived_has_provenance m hne
  exact ⟨e, hs, hc, ⟨mt, ht, (provenance_pins_target e m mt hs ht hc).1 hk⟩⟩

/-- CN-6b in original shape — every Perspective has A-provenance. -/
theorem perspective_has_provenance :
    ∀ m : Memory, memory_kind m = .Perspective →
      ∃ e : Edge, edge_source e = .memory m ∧
        relation_class (edge_relation e) = .Provenance ∧
        (∃ mt : Memory, edge_target e = .memory mt ∧ memory_kind mt = .Abstraction) := by
  intro m hk
  have hne : memory_kind m ≠ .Fact := by rw [hk]; intro h; exact (nomatch h)
  obtain ⟨e, hs, hc, ⟨mt, ht⟩⟩ := derived_has_provenance m hne
  exact ⟨e, hs, hc, ⟨mt, ht, (provenance_pins_target e m mt hs ht hc).2 hk⟩⟩

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
    source batch. OWNER-CONDITIONED (minimization pass): batch ids
    are unique only within `(source_id, owner)` (doc 01 Q6), so the
    gate's scope carries the owner dimension explicitly — without it
    the axiom would identify Abstractions across Owners whose sources
    coincidentally declared the same batch UUID. Decision:
    `docs/domain/decisions/2026-06-11-batch-id-scope.md`. -/
axiom ftoa_batch_exclusive :
  ∀ m1 m2 : Memory,
    memory_kind m1 = .Abstraction → memory_kind m2 = .Abstraction →
    memory_owner m1 = memory_owner m2 →
    memory_source_batch m1 ≠ none →
    memory_source_batch m1 = memory_source_batch m2 →
    memory_input_contract m1 = memory_input_contract m2 →
    memory_operator m1 = memory_operator m2 →
    memory_schema m1 = memory_schema m2 →
    m1 = m2

end Causa
