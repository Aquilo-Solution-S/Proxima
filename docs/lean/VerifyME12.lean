import Causa.Edges

namespace Causa

/-- Proposed residue predicate (shape disjunction WITHOUT the same-kind
    conjunct), taken as hypothesis. Verify: full ME-12 is derivable
    from residue + edge_class_legal (ME-11). -/
theorem me12_from_residue
    (residue : ∀ e : Edge,
      EdgeHasClass e .Supersession →
      ((∃ ms mt : Memory, edge_source e = .memory ms ∧ edge_target e = .memory mt) ∨
       (∃ gs gt : Goal, edge_source e = .goal gs ∧ edge_target e = .goal gt))) :
    ∀ e : Edge, EdgeCoreValid e → EdgeHasClass e .Supersession →
      ((∃ ms mt : Memory, edge_source e = .memory ms ∧ edge_target e = .memory mt ∧
          memory_kind ms = memory_kind mt) ∨
       (∃ gs gt : Goal, edge_source e = .goal gs ∧ edge_target e = .goal gt)) := by
  intro e hvalid hsup
  cases residue e hsup with
  | inr hg => exact .inr hg
  | inl hm =>
    have ⟨ms, mt, hs, ht⟩ := hm
    have hmem := edge_class_legal e .Supersession hsup ms mt hs ht
    cases hks : memory_kind ms <;> cases hkt : memory_kind mt <;>
      rw [hks, hkt] at hmem <;>
      first
        | exact hmem.elim
        | exact .inl ⟨ms, mt, hs, ht, hks.trans hkt.symm⟩
        | exact absurd hmem (by intro h; rcases h with h | h | h | h <;> nomatch h)

end Causa
