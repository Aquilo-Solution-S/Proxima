/-
Causa — Provenance grounding

N1: every memory is grounded by a well-founded derivation /
supersession descent. Temporal monotonicity is a companion law, not
the acyclicity source.
-/

import Causa.Operators

namespace Causa

/-- Descent through Provenance or Supersession. `m'` sits below `m`:
    `m` derives from, or supersedes, `m'`. -/
def derivesFrom (m m' : Memory) : Prop :=
  ∃ e : Edge, edge_source e = .memory m ∧ edge_target e = .memory m' ∧
    (relation_class (edge_relation e) = .Provenance ∨
     relation_class (edge_relation e) = .Supersession)

/-- N1 structural grounding: universal accessibility for the
    descent relation. This is the acyclicity / no-infinite-descent
    law; it is independent of timestamps. -/
axiom grounding_wf : WellFounded (fun lo hi => derivesFrom hi lo)

/-- N1 temporal companion: lower memories are not newer than the
    memories deriving from or superseding them. -/
axiom derivation_created_at_monotone :
  ∀ m m' : Memory, derivesFrom m m' → memory_created_at m' ≤ memory_created_at m

/-- A memory is grounded when repeated descent reaches Facts. -/
inductive GroundsInFact : Memory → Prop where
  | fact {m} : memory_kind m = .Fact → GroundsInFact m
  | step {m m'} : derivesFrom m m' → GroundsInFact m' → GroundsInFact m

/-- N1 bottoms-out theorem: well-founded descent plus per-derived-row
    provenance entails Fact grounding for every memory. -/
theorem memory_grounds_in_facts : ∀ m : Memory, GroundsInFact m := by
  intro m
  exact grounding_wf.induction m
    (fun m ih => by
      by_cases hfact : memory_kind m = .Fact
      · exact GroundsInFact.fact hfact
      · obtain ⟨e, hs, hc, ⟨mt, ht⟩⟩ := derived_has_provenance m hfact
        have hder : derivesFrom m mt := by
          exact ⟨e, hs, ht, Or.inl hc⟩
        exact GroundsInFact.step hder (ih mt hder))

/-- A3 recovery: Abstractions inherit the global Fact-grounding
    theorem. -/
theorem abstraction_grounds_in_facts :
    ∀ m : Memory, memory_kind m = .Abstraction → GroundsInFact m :=
  fun m _ => memory_grounds_in_facts m

end Causa
