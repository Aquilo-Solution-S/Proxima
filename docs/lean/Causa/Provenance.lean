/-
Causa — Provenance grounding

N1: persisted memories are grounded by a well-founded derivation /
supersession descent inside an admitted memory graph. Acyclicity is NOT a
global axiom over raw values: it follows from the graph's table-scoped strict
arrow of time.
-/

import Causa.Operators

namespace Causa

/-- Endpoint membership in the admitted graph tables. A FactEntity endpoint
    must name an admitted aggregate whose current Fact head is an admitted
    memory row. -/
def NodeRefInTables
    (memories : Set Memory) (goals : Set Goal) (factEntities : Set FactEntity) : NodeRef → Prop
  | .memory m => m ∈ memories
  | .goal g   => g ∈ goals
  | .factEntity e => e ∈ factEntities ∧ e.current.memory ∈ memories

/-- Descent through a persisted Provenance or Supersession edge. `m'` sits below
    `m`: `m` derives from, or supersedes, `m'`. The edge must be present in the
    admitted Edge table. -/
def derivesFrom (registry : RelationRegistry) (edges : Set Edge) (m m' : Memory) : Prop :=
  ∃ e : Edge, e ∈ edges ∧ edge_source e = .memory m ∧ edge_target e = .memory m' ∧
    (EdgeHasClass registry e .Provenance ∨ EdgeHasClass registry e .Supersession)

/-- The same descent relation, restricted to memories present in the admitted
    Memory table. -/
def derivesFromInTable
    (registry : RelationRegistry) (memories : Set Memory) (edges : Set Edge)
    (m m' : Memory) : Prop :=
  m ∈ memories ∧ m' ∈ memories ∧ derivesFrom registry edges m m'

/-- P5 / CN-6 — one admitted memory graph under one frozen relation registry.
    Provenance is a table/store invariant: raw Lean `Memory` values do not have
    to carry provenance unless admitted by this bundle. -/
structure MemoryGraphValid
    (registry : RelationRegistry) (memories : Set Memory) (goals : Set Goal)
    (factEntities : Set FactEntity) (edges : Set Edge) : Prop where
  memoryIdUnique : MemoryIdUnique memories
  goalIdUnique : GoalIdUnique goals
  factEntityIdUnique : FactEntityIdUnique factEntities
  factEntityNaturalKeyUnique : FactEntityNaturalKeyUnique factEntities
  factEntityHeadsPresent :
    ∀ e : FactEntity, e ∈ factEntities → e.current.memory ∈ memories
  edgeTableValid : EdgeTableValid registry edges
  edgeEndpointsPresent :
    ∀ e : Edge, e ∈ edges →
      NodeRefInTables memories goals factEntities (edge_source e) ∧
      NodeRefInTables memories goals factEntities (edge_target e)
  derivedProvenance :
    ∀ m : Memory, m ∈ memories → memory_kind m ≠ .Fact →
      ∃ e : Edge, e ∈ edges ∧ EdgeHasClass registry e .Provenance ∧
        edge_source e = .memory m ∧
        (∃ mt : Memory, mt ∈ memories ∧ edge_target e = .memory mt)
  derivationTimeStrict :
    ∀ m m' : Memory, m ∈ memories → m' ∈ memories →
      derivesFrom registry edges m m' → memory_created_at m' < memory_created_at m

/-- Registered edge validity is a projection from graph validity. -/
theorem memory_graph_edge_valid :
    ∀ registry memories goals factEntities edges,
      MemoryGraphValid registry memories goals factEntities edges →
      EdgeTableValid registry edges := by
  intro _ _ _ _ _ hgraph
  exact hgraph.edgeTableValid

/-- N1 structural grounding: the admitted-table descent relation is well-founded
    because it strictly decreases `created_at`. -/
theorem grounding_wf
    (registry : RelationRegistry) (memories : Set Memory) (goals : Set Goal)
    (factEntities : Set FactEntity) (edges : Set Edge)
    (hgraph : MemoryGraphValid registry memories goals factEntities edges) :
    WellFounded (fun lo hi => derivesFromInTable registry memories edges hi lo) :=
  Subrelation.wf
    (fun {a b} h => hgraph.derivationTimeStrict b a h.1 h.2.1 h.2.2)
    (invImage memory_created_at Nat.lt_wfRel).wf

/-- A memory is grounded when repeated admitted descent reaches Facts. -/
inductive GroundsInFact (registry : RelationRegistry) (edges : Set Edge) : Memory → Prop where
  | fact {m} : memory_kind m = .Fact → GroundsInFact registry edges m
  | step {m m'} : derivesFrom registry edges m m' →
      GroundsInFact registry edges m' → GroundsInFact registry edges m

/-- N1 bottoms-out theorem: table-scoped well-founded descent plus per-derived-row
    persisted provenance entails Fact grounding for every admitted memory. -/
theorem memory_grounds_in_facts
    (registry : RelationRegistry) (memories : Set Memory) (goals : Set Goal)
    (factEntities : Set FactEntity) (edges : Set Edge)
    (hgraph : MemoryGraphValid registry memories goals factEntities edges) :
    ∀ m : Memory, m ∈ memories → GroundsInFact registry edges m := by
  intro m
  refine (grounding_wf registry memories goals factEntities edges hgraph).induction
    (C := fun m => m ∈ memories → GroundsInFact registry edges m) m ?_
  intro m ih hm
  by_cases hfact : memory_kind m = .Fact
  · exact GroundsInFact.fact hfact
  · obtain ⟨e, he, hc, hs, ⟨mt, hmt, ht⟩⟩ :=
      hgraph.derivedProvenance m hm hfact
    have hder : derivesFrom registry edges m mt := by
      exact ⟨e, he, hs, ht, Or.inl hc⟩
    have htable : derivesFromInTable registry memories edges m mt := by
      exact ⟨hm, hmt, hder⟩
    exact GroundsInFact.step hder (ih mt htable hmt)

/-- CN-6a in original shape, now table-scoped — every admitted Abstraction has
    persisted Provenance to an admitted Fact or Abstraction. -/
theorem abstraction_has_provenance :
    ∀ registry memories goals factEntities edges,
      MemoryGraphValid registry memories goals factEntities edges →
      ∀ m : Memory, m ∈ memories → memory_kind m = .Abstraction →
        ∃ e : Edge, e ∈ edges ∧ EdgeHasClass registry e .Provenance ∧
          edge_source e = .memory m ∧
          (∃ mt : Memory, mt ∈ memories ∧ edge_target e = .memory mt ∧
            (memory_kind mt = .Fact ∨ memory_kind mt = .Abstraction)) := by
  intro registry memories goals factEntities edges hgraph m hm hk
  have hne : memory_kind m ≠ .Fact := by rw [hk]; intro h; exact (nomatch h)
  obtain ⟨e, he, hc, hs, ⟨mt, hmt, ht⟩⟩ := hgraph.derivedProvenance m hm hne
  exact ⟨e, he, hc, hs,
    ⟨mt, hmt, ht, (provenance_pins_target registry e hc m mt hs ht).1 hk⟩⟩

/-- CN-6b in original shape, now table-scoped — every admitted Perspective has
    persisted Provenance to an admitted Abstraction. -/
theorem perspective_has_provenance :
    ∀ registry memories goals factEntities edges,
      MemoryGraphValid registry memories goals factEntities edges →
      ∀ m : Memory, m ∈ memories → memory_kind m = .Perspective →
        ∃ e : Edge, e ∈ edges ∧ EdgeHasClass registry e .Provenance ∧
          edge_source e = .memory m ∧
          (∃ mt : Memory, mt ∈ memories ∧ edge_target e = .memory mt ∧
            memory_kind mt = .Abstraction) := by
  intro registry memories goals factEntities edges hgraph m hm hk
  have hne : memory_kind m ≠ .Fact := by rw [hk]; intro h; exact (nomatch h)
  obtain ⟨e, he, hc, hs, ⟨mt, hmt, ht⟩⟩ := hgraph.derivedProvenance m hm hne
  exact ⟨e, he, hc, hs,
    ⟨mt, hmt, ht, (provenance_pins_target registry e hc m mt hs ht).2 hk⟩⟩

/-- A3 recovery: admitted Abstractions inherit the table-scoped Fact-grounding
    theorem under the same registered relation vocabulary. -/
theorem abstraction_grounds_in_facts :
    ∀ registry memories goals factEntities edges,
      MemoryGraphValid registry memories goals factEntities edges →
      ∀ m : Memory, m ∈ memories → memory_kind m = .Abstraction →
        GroundsInFact registry edges m := by
  intro registry memories goals factEntities edges hgraph m hm _
  exact memory_grounds_in_facts registry memories goals factEntities edges hgraph m hm

end Causa
