/-
Causa — Provenance grounding

N1: every memory is grounded by a well-founded derivation /
supersession descent. Acyclicity is NOT assumed — it FOLLOWS from the strict
arrow of time: provenance and supersession strictly advance `created_at`, and
Nat time is well-founded. One temporal axiom; the grounding is its theorem.
-/

import Causa.Operators

namespace Causa

/-- Descent through Provenance or Supersession. `m'` sits below `m`:
    `m` derives from, or supersedes, `m'`. -/
def derivesFrom (m m' : Memory) : Prop :=
  ∃ e : Edge, edge_source e = .memory m ∧ edge_target e = .memory m' ∧
    (EdgeHasClass e .Provenance ∨ EdgeHasClass e .Supersession)

/-- N1 — the single grounding law: what a memory derives from or is superseded
    by was created STRICTLY earlier. Provenance and supersession advance forward
    in time — you cannot derive from a memory that does not yet exist, and a new
    version supersedes an older one. This strictness is the whole of N1. -/
axiom derivation_created_at_strict :
  ∀ m m' : Memory, derivesFrom m m' → memory_created_at m' < memory_created_at m

/-- N1 structural grounding: the descent relation is well-founded (acyclic, no
    infinite descent). THEOREM, no longer an axiom — descent strictly decreases
    the `created_at` instant, and `<` on `Nat` is well-founded, so its inverse
    image under `memory_created_at` is too. The arrow of time grounds the graph. -/
theorem grounding_wf : WellFounded (fun lo hi => derivesFrom hi lo) :=
  Subrelation.wf
    (fun {a b} h => derivation_created_at_strict b a h)
    (invImage memory_created_at Nat.lt_wfRel).wf

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
      · obtain ⟨e, hc, hs, ⟨mt, ht⟩⟩ := derived_has_provenance m hfact
        have hder : derivesFrom m mt := by
          exact ⟨e, hs, ht, Or.inl hc⟩
        exact GroundsInFact.step hder (ih mt hder))

/-- A3 recovery: Abstractions inherit the global Fact-grounding
    theorem. -/
theorem abstraction_grounds_in_facts :
    ∀ m : Memory, memory_kind m = .Abstraction → GroundsInFact m :=
  fun m _ => memory_grounds_in_facts m

end Causa
